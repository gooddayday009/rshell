# RShell Agent SDK component

This component uses the official `claude-agent-sdk` Python package. It is a thin protocol adapter, not a reimplementation of the agent loop. The SDK and its bundled engine run as child processes; the Rust host owns terminal access, session scope, and human command approval.

The application remains Rust. This small Python SDK boundary is the exception required by the user's explicit choice of the official Python/TypeScript SDK. There is no Web frontend. PyInstaller packages the Python runtime, dependencies and bundled engine so end users do not need Python or an independently installed CLI. Offline packages keep the entire `agent-sdk` directory beside `rshell.exe`. Lightweight builds ask the user to download a pinned component or select an existing local rshell-agent-sdk.exe when executing Agent mode needs a component; choosing Download installs into the per-user RShell components cache.

The adapter calls `claude_agent_sdk.query()` with built-in tools disabled, only the scoped RShell MCP server, `dontAsk` permission mode and isolated settings. The MCP server's `execute_command` is a proposal: Rust must obtain explicit human approval and reviewed output. Windows Job Objects kill the entire SDK process tree on cancellation or application exit.

Authentication uses API credentials (`ANTHROPIC_API_KEY`, or a compatible endpoint's `ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_BASE_URL`). The host may inherit only the documented auth/model/proxy environment allowlist from existing user settings; it does not import hooks, plugins, permissions or subscription-login credentials. No key is bundled, copied into the RShell configuration, or written to the bridge request. Credentials are passed through the child environment. Other providers/authentication methods require separate verification.

The RShell UI now selects a complete saved model profile: Anthropic base URL, model ID and its DPAPI-protected API key. In this path, the host clears inherited provider credentials/model aliases, preserves only basic operating-system and proxy environment values, and does not read user Claude settings. The legacy environment-auth path above remains only for the opt-in standalone live probe. OpenAI profiles can be used for question/command-suggestion chat but cannot directly drive the SDK execution mode. Use `claude_agent_probe --loopback-profile` to check the packaged SDK against a synthetic local API without contacting a real provider.

The upstream SDK and its bundled engine have their own licensing and commercial terms; they are not relicensed under RShell's license. Dependency metadata/licenses are included in `_internal`, the Python license is in `licenses`, and `manifest.json` records packaged dependency versions and file hashes.

Official documentation: https://code.claude.com/docs/en/agent-sdk/overview

## Development

Run `scripts/build-agent-sdk.ps1 -InstallDependencies` with Python 3.10+ x64 to build `target/debug/agent-sdk`. Subsequent builds can omit `-InstallDependencies`. Build caches and intermediate files stay under `target`. Release/installer scripts omit this directory by default; pass `-WithAgent` to create an offline full package. The SDK self-test checks the bundled engine without making a model request. The Rust `claude_agent_probe` example is an opt-in live model/MCP test using synthetic terminal data and rejecting all proposed commands.
