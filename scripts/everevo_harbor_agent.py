"""Harbor adapter for the EverEvo agent — Terminal-Bench 2.0 evaluation.

Runs the EverEvo agent loop inside the task container via the `chat` CLI
subcommand (`everevo-server chat "<task>"`), which reads the LLM config from
`data/config.toml`, spins up the loop (AgentRun::cli), and returns the final
text answer.

Based on the official Harbor installed-agent API (`BaseInstalledAgent`):
- `install(environment)` — upload the Linux binary + config into the container
- `run(instruction, environment, context)` — execute the agent headlessly
- `populate_context_post_run(context)` — surface the answer to Harbor

Usage:
    harbor run -d terminal-bench@2.0 \
        --agent-import-path scripts.everevo_harbor_agent:EverEvoAgent \
        --model deepseek-v4-flash -l 1
"""

import os
import re
import shlex
from pathlib import Path

from harbor.agents.installed.base import BaseInstalledAgent, with_prompt_template
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext

# Repo-root-relative paths (harbor runs from the EverEvo workspace root).
_BINARY = Path("target/release/everevo-server")
# The live LLM config — read at install time and uploaded into the container,
# NEVER copied into the tracked repo tree (contains an API key; gitignored).
_CONFIG = Path("data/config.toml")

# Container paths.
_CONTAINER_BINARY = "/usr/local/bin/everevo-server"
_CONTAINER_DATA = "/data"
_CONTAINER_LOG = "/logs/agent/everevo.log"


class EverEvoAgent(BaseInstalledAgent):
    @staticmethod
    def name() -> str:
        return "everevo"

    def get_version_command(self) -> str | None:
        return "everevo-server --help"

    async def install(self, environment: BaseEnvironment) -> None:
        """Copy the Linux binary + LLM config into the container.

        Also installs `ca-certificates`: minimal images (e.g. `ubuntu:24.04`)
        ship without it and the EverEvo LLM TLS handshake fails with
        `Connection failed` until it's present (verified 2026-08-13). Non-apt
        images (alpine etc.) usually bundle certs already; the install is
        best-effort and skips if `apt-get` is unavailable.
        """
        await self.exec_as_root(environment, command="mkdir -p /usr/local/bin /data /logs/agent")
        # Best-effort TLS trust store — only on minimal apt images that lack it
        # (ubuntu:24.04 breaks EverEvo's LLM TLS with `Connection failed` until
        # ca-certificates is installed; verified 2026-08-13).
        await self.exec_as_root(
            environment,
            command=(
                "[ -f /etc/ssl/certs/ca-certificates.crt ] || "
                "{ command -v apt-get >/dev/null 2>&1 && "
                "(apt-get update -qq && apt-get install -y -qq ca-certificates) "
                ">/dev/null 2>&1; } || true"
            ),
        )
        await environment.upload_file(str(_BINARY), _CONTAINER_BINARY)
        await self.exec_as_root(
            environment,
            command=f"chmod 755 {_CONTAINER_BINARY} && {_CONTAINER_BINARY} --help",
        )
        if _CONFIG.exists():
            await environment.upload_file(str(_CONFIG), f"{_CONTAINER_DATA}/config.toml")
        else:
            raise FileNotFoundError(
                f"{_CONFIG} missing — the EverEvo LLM config (data/config.toml) must "
                "exist for the containerized agent to reach its model backend"
            )

    @with_prompt_template
    async def run(self, instruction: str, environment: BaseEnvironment, context: AgentContext) -> None:
        """Run the EverEvo agent loop against the task instruction.

        The agent's shell must operate directly on the task container's
        filesystem (Terminal-Bench scores the final container state, not an
        answer string), so `EVEREVO_SANDBOX_ROOT` points the loop's shell at
        the container working directory and we run with that cwd.
        """
        quoted = shlex.quote(instruction)
        # Harbor runs installed agents in the task WORKDIR (break-filter → /app);
        # the EverEvo CLI reads config.toml from EVEREVO_DATA_DIR and the loop's
        # shell targets EVEREVO_SANDBOX_ROOT. Temperature is fixed at 0.0 by the
        # LLM config (anti-cheating requirement).
        command = (
            f"EVEREVO_DATA_DIR=/data "
            f"EVEREVO_SANDBOX_ROOT=$PWD "
            f"EVEREVO_SANDBOX_UNRESTRICTED=1 "
            # EverEvo runs unbounded (EVEREVO_MAX_TURNS unset → 0 = unlimited);
            # the task's own `agent.timeout_sec` (official cap, e.g. 900s)
            # bounds each run — the reference-value comparison baseline.
            f"{_CONTAINER_BINARY} chat {quoted} "
            f"2>&1 | tee {_CONTAINER_LOG}"
        )
        await self.exec_as_agent(environment, command=command)

    def populate_context_post_run(self, context: AgentContext) -> None:
        """Backfill token/cost metadata from the agent log.

        No answer to surface — Terminal-Bench scores the container state. The
        EverEvo CLI prints a stable `__TOKENS__ <in> <out>` line at the end of
        each run; parse it and price at the deepseek-v4-flash rate (input
        $0.14/1M, output $0.28/1M; no cache-hit split available from the CLI).
        """
        log = Path(_CONTAINER_LOG)
        if not log.exists():
            return
        text = log.read_text(errors="replace")
        m = re.search(r"__TOKENS__ (\d+) (\d+)", text)
        if not m:
            return
        in_tok, out_tok = int(m.group(1)), int(m.group(2))
        # deepseek-v4-flash pricing (USD per 1M tokens)
        cost = in_tok / 1e6 * 0.14 + out_tok / 1e6 * 0.28
        context.n_input_tokens = in_tok
        context.n_output_tokens = out_tok
        context.cost_usd = round(cost, 6)
