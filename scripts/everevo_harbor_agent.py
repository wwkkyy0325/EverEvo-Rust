#!/usr/bin/env python3
"""
EverEvo Harbor Agent Adapter for Terminal-Bench 2.0.

This adapter makes EverEvo (the AGENT, not just the LLM) runnable as a Harbor
agent, so it can be evaluated on Terminal-Bench 2.0's 89 real-world terminal tasks.

Architecture:
    Harbor → Docker container → everevo-server (HTTP) → Agent Loop → 22 tools
                                        ↑
    Task instruction sent via POST /api/chat (SSE), agent's bash/read_file/
    write_file tools run naturally on the container filesystem.

Usage:
    harbor run --dataset terminal-bench@2.0 \\
      --agent scripts.everevo_harbor_agent:EverEvoAgent \\
      --model glm-5.2 \\
      --n-concurrent 4
"""

import asyncio
import json
import os
import shlex
import signal
import sys
import time
from pathlib import Path, PurePosixPath
from typing import Any, ClassVar, override

import aiohttp

from harbor.agents.installed.base import (
    BaseInstalledAgent,
    CliFlag,
    NonZeroAgentExitCodeError,
    with_prompt_template,
)
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext
from harbor.models.agent.name import AgentName

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

# Paths inside the Docker container
_REMOTE_AGENT_DIR = PurePosixPath("/installed-agent")
_REMOTE_BINARY = _REMOTE_AGENT_DIR / "everevo-server"
_REMOTE_CONFIG = _REMOTE_AGENT_DIR / "config.toml"
_REMOTE_DATA_DIR = _REMOTE_AGENT_DIR / "data"
_REMOTE_LOG = _REMOTE_AGENT_DIR / "everevo-server.log"

# Default server config
_DEFAULT_PORT = 13456
_SERVER_STARTUP_TIMEOUT = 60  # seconds to wait for /api/health
_AGENT_TIMEOUT = 300  # seconds max for agent to complete task

# ---------------------------------------------------------------------------
# EverEvo Agent
# ---------------------------------------------------------------------------


class EverEvoAgent(BaseInstalledAgent):
    """Run EverEvo agent inside the Docker container to complete terminal tasks.

    The agent loop is entirely inside everevo-server:
      1. Server starts inside the container
      2. Task instruction is sent via POST /api/chat (SSE)
      3. Agent thinks, calls tools (bash/read_file/write_file) on the container FS
      4. Tools execute naturally inside the container
      5. Agent responds, server stops, Harbor verifier checks the result
    """

    CLI_FLAGS: ClassVar[list[CliFlag]] = [
        CliFlag("port", cli="--port", type="int", default=_DEFAULT_PORT),
        CliFlag("startup_timeout", cli="--startup-timeout", type="int", default=_SERVER_STARTUP_TIMEOUT),
        CliFlag("agent_timeout", cli="--agent-timeout", type="int", default=_AGENT_TIMEOUT),
    ]

    _BINARY_SOURCE: ClassVar[str | None] = None  # Set from __init__
    _CONFIG_SOURCE: ClassVar[str | None] = None  # Set from __init__

    def __init__(
        self,
        *args: Any,
        binary_path: str | Path | None = None,
        config_path: str | Path | None = None,
        port: int = _DEFAULT_PORT,
        startup_timeout: int = _SERVER_STARTUP_TIMEOUT,
        agent_timeout: int = _AGENT_TIMEOUT,
        **kwargs: Any,
    ) -> None:
        super().__init__(*args, **kwargs)
        self._binary_host_path = Path(binary_path) if binary_path else None
        self._config_host_path = Path(config_path) if config_path else None
        self._port = port
        self._startup_timeout = startup_timeout
        self._agent_timeout = agent_timeout

    # ---- Agent metadata ---------------------------------------------------

    @staticmethod
    @override
    def name() -> str:
        return "everevo"

    @override
    def get_version_command(self) -> str | None:
        return f"{shlex.quote(_REMOTE_BINARY.as_posix())} --version"

    @override
    def parse_version(self, stdout: str) -> str:
        return stdout.strip().split()[-1] if stdout.strip() else "unknown"

    # ---- Install ----------------------------------------------------------

    @override
    async def install(self, environment: BaseEnvironment) -> None:
        """Copy everevo-server binary and config into the container."""
        self.logger.info("Installing EverEvo agent in container...")

        # 1. Install system deps (nothing special needed — everevo-server is static-ish)
        await self.exec_as_root(
            environment,
            command=(
                "if command -v apt-get >/dev/null 2>&1; then "
                "apt-get update && apt-get install -y curl ca-certificates; "
                "elif command -v apk >/dev/null 2>&1; then "
                "apk add --no-cache curl ca-certificates bash; "
                "fi"
            ),
            env={"DEBIAN_FRONTEND": "noninteractive"},
        )

        # 2. Create directories
        await self.exec_as_root(
            environment,
            command=(
                f"mkdir -p {shlex.quote(_REMOTE_AGENT_DIR.as_posix())} "
                f"{shlex.quote(_REMOTE_DATA_DIR.as_posix())} /logs/agent"
            ),
        )

        # 3. Upload everevo-server binary
        binary_src = self._find_binary()
        self.logger.info(f"Uploading binary: {binary_src}")
        await environment.upload_file(
            binary_src,
            _REMOTE_BINARY.as_posix(),
        )
        await self.exec_as_root(
            environment,
            command=f"chmod +x {shlex.quote(_REMOTE_BINARY.as_posix())}",
        )

        # 4. Upload config with API keys
        config_src = self._find_config()
        self.logger.info(f"Uploading config: {config_src}")
        await environment.upload_file(
            config_src,
            _REMOTE_CONFIG.as_posix(),
        )

        # 5. Set agent user ownership
        agent_user = str(environment.default_user or "agent")
        quoted_user = shlex.quote(agent_user)
        await self.exec_as_root(
            environment,
            command=(
                f"chown -R {quoted_user}:{quoted_user} "
                f"{shlex.quote(_REMOTE_AGENT_DIR.as_posix())}"
            ),
        )

        # 6. Verify binary works
        try:
            result = await environment.exec(
                command=f"{shlex.quote(_REMOTE_BINARY.as_posix())} --version",
            )
            self.logger.info(f"Binary version: {result.stdout.strip()}")
        except Exception as exc:
            self.logger.warning(f"Version check failed (non-fatal): {exc}")

    def _find_binary(self) -> Path:
        """Resolve the everevo-server binary path."""
        if self._binary_host_path and self._binary_host_path.exists():
            return self._binary_host_path

        # Auto-detect: search workspace root
        candidates = [
            Path(os.getcwd()) / "target" / "release" / "everevo-server",
            Path(os.getcwd()) / "target" / "x86_64-unknown-linux-gnu" / "release" / "everevo-server",
        ]
        for candidate in candidates:
            if candidate.exists():
                return candidate

        raise FileNotFoundError(
            "Cannot find everevo-server binary. "
            "Build it with: cargo build -p everevo-server --release "
            "Or pass --ak binary_path=/path/to/everevo-server"
        )

    def _find_config(self) -> Path:
        """Resolve the config.toml path."""
        if self._config_host_path and self._config_host_path.exists():
            return self._config_host_path

        candidates = [
            Path(os.getcwd()) / "data" / "config.toml",
            Path(os.getcwd()) / "data" / "config" / "config.toml",
        ]
        for candidate in candidates:
            if candidate.exists():
                return candidate

        raise FileNotFoundError(
            "Cannot find config.toml. "
            "Pass --ak config_path=/path/to/config.toml"
        )

    # ---- Run --------------------------------------------------------------

    @override
    @with_prompt_template
    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        """Run EverEvo agent to complete the terminal task."""
        self.logger.info(f"Running EverEvo agent on task: {instruction[:100]}...")

        server_process = None
        try:
            # 1. Start everevo-server in background
            server_process = await self._start_server(environment)

            # 2. Send task instruction via chat API
            result = await self._run_agent(instruction, environment)

            # 3. Record results
            context.n_input_tokens = result.get("input_tokens", 0)
            context.n_output_tokens = result.get("output_tokens", 0)

            if result.get("error"):
                self.logger.error(f"Agent error: {result['error']}")

            self.logger.info(
                f"Agent completed. "
                f"Input tokens: {context.n_input_tokens}, "
                f"Output tokens: {context.n_output_tokens}"
            )

        finally:
            # 4. Stop server
            if server_process is not None:
                await self._stop_server(environment)

    async def _start_server(self, environment: BaseEnvironment) -> Any:
        """Start everevo-server in background inside the container."""
        data_dir = shlex.quote(_REMOTE_DATA_DIR.as_posix())
        binary = shlex.quote(_REMOTE_BINARY.as_posix())
        config = shlex.quote(_REMOTE_CONFIG.as_posix())
        log = shlex.quote(_REMOTE_LOG.as_posix())

        # The server reads config from EVEREVO_CONFIG env var
        env = {
            "EVEREVO_CONFIG": _REMOTE_CONFIG.as_posix(),
            "EVEREVO_DATA_DIR": _REMOTE_DATA_DIR.as_posix(),
        }
        # Pass API keys from agent env (configured via --ae or .env)
        for key in ("ANTHROPIC_API_KEY", "OPENAI_API_KEY", "DEEPSEEK_API_KEY"):
            if key in self._extra_env:
                env[key] = self._extra_env[key]

        # Build env string for shell
        env_str = " ".join(f"{k}={shlex.quote(v)}" for k, v in env.items())

        # Start server in background
        # Use nohup + background; log output for debugging
        start_cmd = (
            f"set -o pipefail; "
            f"{env_str} "
            f"nohup {binary} serve "
            f"--host 127.0.0.1 --port {self._port} "
            f"> {log} 2>&1 & "
            f"echo $!"
        )

        result = await self.exec_as_agent(environment, command=start_cmd)
        pid = result.stdout.strip()
        self.logger.info(f"Server started with PID {pid}")

        # Wait for health check
        health_url = f"http://127.0.0.1:{self._port}/api/health"
        for i in range(self._startup_timeout):
            try:
                result = await self.exec_as_agent(
                    environment,
                    command=f"curl -sf {shlex.quote(health_url)}",
                    timeout_sec=5,
                )
                if "ok" in (result.stdout or "").lower() or result.return_code == 0:
                    self.logger.info(f"Server healthy after {i + 1}s")
                    return pid
            except Exception:
                pass
            await asyncio.sleep(1)

        # If we get here, server didn't start — dump logs
        try:
            result = await environment.exec(command=f"cat {_REMOTE_LOG.as_posix()}")
            self.logger.error(f"Server startup failed. Log:\n{result.stdout}")
        except Exception:
            pass
        raise RuntimeError(f"Server failed to start within {self._startup_timeout}s")

    async def _run_agent(self, instruction: str, environment: BaseEnvironment) -> dict:
        """Send task to everevo-server via chat API and collect SSE response."""
        port = self._port
        api_url = f"http://127.0.0.1:{port}/api/chat"

        # Escape the instruction for JSON
        body = json.dumps({"message": instruction})

        # Use curl to POST to chat API with SSE streaming
        # We write the full SSE stream to a log file for analysis
        sse_log = _REMOTE_AGENT_DIR / "sse-output.txt"
        sse_log_quoted = shlex.quote(sse_log.as_posix())

        curl_cmd = (
            f"curl -sS -N --max-time {self._agent_timeout} "
            f"-X POST {shlex.quote(api_url)} "
            f"-H 'Content-Type: application/json' "
            f"-d {shlex.quote(body)} "
            f"> {sse_log_quoted} 2>&1"
        )

        try:
            await self.exec_as_agent(
                environment,
                command=curl_cmd,
                timeout_sec=self._agent_timeout + 10,
            )
        except NonZeroAgentExitCodeError as e:
            # curl exits non-zero on timeout, which is expected
            self.logger.warning(f"curl exited non-zero (may be timeout): {e}")

        # Parse the SSE log
        result = await self._parse_sse_log(environment, sse_log)
        return result

    async def _parse_sse_log(
        self, environment: BaseEnvironment, sse_log: PurePosixPath
    ) -> dict:
        """Parse the SSE output log to extract agent response and token counts."""
        result = {
            "text": "",
            "tool_calls": [],
            "input_tokens": 0,
            "output_tokens": 0,
            "error": None,
        }

        try:
            exec_result = await environment.exec(
                command=f"cat {shlex.quote(sse_log.as_posix())}"
            )
            output = exec_result.stdout or ""
        except Exception:
            return result

        if not output:
            result["error"] = "Empty SSE output"
            return result

        # Parse SSE lines
        current_event = ""
        for line in output.split("\n"):
            line = line.strip()
            if line.startswith("event: "):
                current_event = line[7:].strip()
            elif line.startswith("data: "):
                data_str = line[6:]
                try:
                    data = json.loads(data_str)
                except json.JSONDecodeError:
                    continue

                if current_event == "text":
                    result["text"] += data.get("content", "")
                elif current_event in ("tool_call_start", "tool_call"):
                    name = data.get("name", "?")
                    args = str(data.get("arguments", ""))[:200]
                    result["tool_calls"].append({"name": name, "arguments": args})
                elif current_event == "done":
                    result["input_tokens"] = data.get("input_tokens", 0)
                    result["output_tokens"] = data.get("output_tokens", 0)
                elif current_event == "error":
                    result["error"] = data_str[:500]

        self.logger.info(
            f"Parsed SSE: text={len(result['text'])} chars, "
            f"tools={len(result['tool_calls'])}, "
            f"tokens={result['input_tokens']}/{result['output_tokens']}"
        )
        return result

    async def _stop_server(self, environment: BaseEnvironment) -> None:
        """Stop the everevo-server process."""
        try:
            await self.exec_as_root(
                environment,
                command=(
                    f"pkill -f {shlex.quote(_REMOTE_BINARY.as_posix())} || true"
                ),
                timeout_sec=5,
            )
        except Exception as exc:
            self.logger.debug(f"Server stop: {exc}")
