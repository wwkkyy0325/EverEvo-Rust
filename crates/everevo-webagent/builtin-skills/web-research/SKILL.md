---
name: web-research
description: Multi-engine web search with anti-detection browser + CAPTCHA handling
when_to_use:
  - Search the web for current information
  - Look up error messages, API docs, or library documentation
  - Research a topic that requires up-to-date web knowledge
  - Any question that requires information beyond the model's training data
tools: [web_search, web_fetch, web_browse]
persona: |
  When searching the web, use web_search first (fast HTTP, no browser).
  If web_search returns no results or the page is blocked (anti-bot challenge),
  fall back to web_browse which uses a real browser with anti-detection.
  Use web_fetch to read the full content of a specific URL.
---

# Web Research Skill

You have access to a web research service with three tools:

## web_search — Fast HTTP Search (use first)
- Multi-engine: Bing (cn.bing.com) then DuckDuckGo as fallback
- Returns title, URL, and snippet for each result
- **Use this first** — it's fast and doesn't need a browser
- Example: `web_search(query="rust async trait error", limit=8)`

## web_browse — Stealth Browser Search (use if web_search blocked)
- Launches a real Chrome/Edge browser with anti-detection measures
- Can bypass Cloudflare, reCAPTCHA checkbox, and other basic blocks
- Handles JavaScript rendering
- **Slower but more reliable** — use if web_search returns no results
- Example: `web_browse(query="rust async trait error", limit=8)`

## web_fetch — Read a Specific Page
- Fetch and extract text content from a URL
- Supports both direct HTTP (fast) and browser rendering (for JS pages)
- Use to read the full content of a result from web_search or web_browse
- Example: `web_fetch(url="https://doc.rust-lang.org/book/ch17-01.html")`
- For JavaScript-heavy pages, add `render_js=true`

## Strategy

1. **Start with web_search** — it covers 80% of queries with zero overhead
2. If blocked → **web_browse** with the same query
3. For deep reading → **web_fetch** on the most promising result URLs
4. Report findings with URLs so the user can verify

## Privacy & Safety

- The browser uses a **temporary profile** — no cookies, no login state
- Your personal accounts are **never** exposed to search engines
- Search engines see the browser fingerprint, not your identity
