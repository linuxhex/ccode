"""Web test interface for CCODE agent.

Usage:
    cd python
    uv add --dev fastapi uvicorn
    uvicorn ccode.web_test:app --reload --port 8765

Then open http://localhost:8765 in a browser.
Use /login to configure API key (like Claude Code's /login command).
"""

import json
import os
from pathlib import Path

from fastapi import FastAPI, Request
from fastapi.responses import HTMLResponse, StreamingResponse

from ccode.reasoning.engine import ReasoningEngine
from ccode.tools import ToolExecutor

app = FastAPI(title="CCODE Agent Test")

PROJECT_DIR = Path(os.environ.get("CCODE_PROJECT_DIR", Path(__file__).resolve().parent.parent.parent)).resolve()
MODEL = os.environ.get("CCODE_MODEL", "claude-sonnet-4-20250514")
engine = ReasoningEngine(model=MODEL)
tool_executor = ToolExecutor()

STATIC_DIR = Path(__file__).parent / "static"


def _provider_status() -> dict:
    engine._ensure_initialized()
    configs = engine.router._configs if hasattr(engine.router, "_configs") else {}
    return {
        "anthropic": bool(os.environ.get("ANTHROPIC_API_KEY")) or "claude" in configs,
        "openai": bool(os.environ.get("OPENAI_API_KEY")) or "openai" in configs,
        "gemini": bool(os.environ.get("GEMINI_API_KEY")) or "gemini" in configs,
        "registered": len(configs),
        "providers": list(configs.keys()),
    }


@app.get("/", response_class=HTMLResponse)
async def index():
    html = (STATIC_DIR / "index.html").read_text(encoding="utf-8")
    status = _provider_status()
    return html.replace("__PROVIDER_STATUS__", json.dumps(status))


@app.get("/login", response_class=HTMLResponse)
async def login_page():
    return (STATIC_DIR / "login.html").read_text(encoding="utf-8")


@app.post("/login")
async def login_submit(request: Request):
    body = await request.json()
    provider = body.get("provider", "").strip().lower()
    api_key = body.get("api_key", "").strip()
    if not provider or not api_key:
        return {"success": False, "error": "provider and api_key required"}
    ok = engine.configure_api_key(provider, api_key)
    if ok:
        return {"success": True, "provider": provider}
    return {"success": False, "error": f"unknown provider: {provider}"}


@app.post("/chat")
async def chat(request: Request):
    body = await request.json()
    message = body.get("message", "").strip()
    if not message:
        return {"error": "empty message"}

    engine._ensure_initialized()
    configs = engine.router._configs if hasattr(engine.router, "_configs") else {}
    if not configs:
        async def error_stream():
            err = json.dumps({
                "error": "no_provider",
                "content": (
                    "**Missing API key** — [Click here to configure](/login)"
                ),
            }, ensure_ascii=False)
            yield f"data: {err}\n\n"
            yield "data: [DONE]\n\n"
        return StreamingResponse(error_stream(), media_type="text/event-stream")

    tool_executor_ref = tool_executor

    async def event_stream():
        async for chunk in engine.chat(message, PROJECT_DIR, tool_executor=tool_executor_ref):
            data = json.dumps({"content": chunk}, ensure_ascii=False)
            yield f"data: {data}\n\n"
        yield "data: [DONE]\n\n"

    return StreamingResponse(event_stream(), media_type="text/event-stream")


@app.post("/clear")
async def clear_context():
    engine.context.clear()
    return {"success": True, "message": "conversation cleared"}


@app.get("/context")
async def context_status():
    return {
        "messages": len(engine.context.messages),
        "estimated_tokens": engine.context.estimate_tokens(),
        "max_tokens": engine.context.max_tokens,
    }


@app.get("/health")
async def health():
    return {"status": "ok", "model": MODEL, "project": str(PROJECT_DIR), **_provider_status()}
