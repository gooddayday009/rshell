"""Thin Claude Agent SDK adapter. Terminal tools and approvals live in Rust.

One bounded JSON request on stdin, normalized JSONL events on stdout. This file
is frozen with its Python runtime and the SDK's bundled engine for Windows x64.
"""
import asyncio
import importlib.metadata
import json
import os
from pathlib import Path
import subprocess
import sys

import claude_agent_sdk
from claude_agent_sdk import (
    AssistantMessage, ClaudeAgentOptions, ResultMessage, TextBlock, query,
)

PROTOCOL = 1
MAX_REQUEST = 1_048_576
TOOLS = ["mcp__rshell__read_terminal", "mcp__rshell__execute_command"]


def bundled_engine():
    path = Path(claude_agent_sdk.__file__).parent / "_bundled" / "claude.exe"
    if not path.is_file():
        raise RuntimeError("bundled_engine_missing")
    return path


def make_options(request):
    if request.get("protocol_version") != PROTOCOL:
        raise ValueError("protocol_mismatch")
    prompt = request.get("prompt")
    turns = request.get("max_turns")
    if not isinstance(prompt, str) or len(prompt.encode("utf-8")) > 160_000:
        raise ValueError("invalid_prompt")
    if not isinstance(turns, int) or isinstance(turns, bool) or not 1 <= turns <= 30:
        raise ValueError("invalid_turn_limit")
    servers = request.get("mcp_servers")
    if not isinstance(servers, dict) or set(servers) != {"rshell"}:
        raise ValueError("invalid_mcp_scope")
    return ClaudeAgentOptions(
        cli_path=str(bundled_engine()),  # Never resolve an executable from PATH.
        tools=[],
        allowed_tools=TOOLS,
        permission_mode="dontAsk",
        mcp_servers=servers,
        strict_mcp_config=True,
        setting_sources=[],
        settings=json.dumps({"disableAllHooks": True}),
        cwd=os.getcwd(),
        model=request.get("model") or None,
        max_turns=turns,
        max_buffer_size=MAX_REQUEST,
        env={"CLAUDE_AGENT_SDK_CLIENT_APP": "RShell"},
        extra_args={"no-session-persistence": None, "disable-slash-commands": None, "no-chrome": None},
    )


def emit(value):
    sys.stdout.write(json.dumps(value, ensure_ascii=True) + "\n")
    sys.stdout.flush()


async def run(request):
    options = make_options(request)
    # The SDK manages startup, control messages, streaming and cleanup of its engine.
    async for message in query(prompt=request["prompt"], options=options):
        if isinstance(message, AssistantMessage):
            emit({"type": "assistant", "message": {"content": [
                {"type": "text", "text": block.text[:8000]}
                for block in message.content if isinstance(block, TextBlock)
            ]}})
        elif isinstance(message, ResultMessage):
            emit({"type": "result", "subtype": message.subtype,
                  "is_error": message.is_error, "result": (message.result or "")[:8000]})


def self_test():
    engine = bundled_engine()
    options = make_options({"protocol_version": PROTOCOL, "prompt": "test", "max_turns": 1,
                            "mcp_servers": {"rshell": {"type": "stdio", "command": "unused"}}})
    assert options.tools == [] and options.allowed_tools == TOOLS
    assert options.strict_mcp_config and options.setting_sources == []
    assert options.permission_mode == "dontAsk"
    version = subprocess.run([str(engine), "--version"], capture_output=True, text=True,
                             timeout=15, check=True, creationflags=0x08000000).stdout.strip()
    emit({"type": "sdk_info", "protocol_version": PROTOCOL,
          "sdk_version": importlib.metadata.version("claude-agent-sdk"),
          "engine_version": version, "bundled_engine": True})


def main():
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        return
    # No logging of request data or raw SDK exceptions: they may contain secrets.
    data = sys.stdin.buffer.readline(MAX_REQUEST + 1)
    if len(data) > MAX_REQUEST or not data.endswith(b"\n"):
        raise ValueError("invalid_request_size")
    asyncio.run(run(json.loads(data)))


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        emit({"type": "result", "subtype": "sdk_error", "is_error": True,
              "result": f"Agent SDK failed ({type(error).__name__}); check SDK installation, API authentication and model configuration."})
        sys.exit(1)
