#!/usr/bin/env python3
"""A local model for tests/scenarios/alarms.sh: alarms-model.py <port-file> <rules.json> <log>

Writes its port to <port-file>. Serves the Anthropic Messages API (Claude
Code) and the OpenAI chat completions API (omp). A request whose messages
contain a rule's `match` gets the rule's tool call n (n = tool results so
far), then the text "done"; every other request gets "done". MonoCode's
helper prompts (session titles, commit messages) quote the user's message,
so they can hold a `match`; they say "Do not call tools", and get "done" as
a model that obeys would. rules.json is
[{"match": "...", "tools": [{"name": "Bash", "input": {...}}]}]. Each answer
is a line in <log>.
"""
import json
import sys
from pathlib import Path
from http.server import BaseHTTPRequestHandler
from socketserver import ThreadingTCPServer

RULES = json.loads(Path(sys.argv[2]).read_text())
LOG = Path(sys.argv[3])


def flat(content):
    if isinstance(content, list):
        return " ".join(flat(part.get("text", part.get("content", ""))) for part in content if isinstance(part, dict))
    return str(content or "")


def results(messages, openai):
    """What each tool call returned, by call id, as the agent sent it back."""
    if openai:
        return {m.get("tool_call_id"): flat(m.get("content")) for m in messages if m.get("role") == "tool"}
    found = {}
    for message in messages:
        for block in message.get("content", []) if isinstance(message.get("content"), list) else []:
            if isinstance(block, dict) and block.get("type") == "tool_result":
                found[block.get("tool_use_id")] = ("error: " if block.get("is_error") else "") + flat(block.get("content"))
    return found


def pick(body, openai):
    messages = body.get("messages", [])
    text = json.dumps(messages)
    named = (lambda tool: tool.get("function", {}).get("name")) if openai else (lambda tool: tool.get("name"))
    tools = {named(tool) for tool in body.get("tools", [])}
    if "Do not call tools" in text:
        return None
    returned = results(messages, openai)
    for rule in RULES:
        if rule["match"] not in text or not tools:
            continue
        n = len(returned)
        call = rule["tools"][n] if n < len(rule["tools"]) and rule["tools"][n]["name"] in tools else None
        call = call and {"id": f"toolu_{rule['match'].lower().replace('-', '_')}_{n}", **call}
        with LOG.open("a") as log:
            log.write(json.dumps({"match": rule["match"], "results": returned, "call": call}) + "\n")
        return call
    return None


def anthropic(body):
    call = pick(body, openai=False)
    if call:
        block, stop = {"type": "tool_use", "id": call["id"], "name": call["name"], "input": call["input"]}, "tool_use"
    else:
        block, stop = {"type": "text", "text": "done"}, "end_turn"
    usage = {"input_tokens": 1, "output_tokens": 1}
    message = {"id": "msg_alarms", "type": "message", "role": "assistant", "model": body.get("model", "m"),
               "stop_sequence": None, "usage": usage}
    if not body.get("stream"):
        return "application/json", json.dumps({**message, "content": [block], "stop_reason": stop})
    if block["type"] == "tool_use":
        start, delta = {**block, "input": {}}, {"type": "input_json_delta", "partial_json": json.dumps(block["input"])}
    else:
        start, delta = {"type": "text", "text": ""}, {"type": "text_delta", "text": block["text"]}
    events = [
        ("message_start", {"message": {**message, "content": [], "stop_reason": None}}),
        ("content_block_start", {"index": 0, "content_block": start}),
        ("content_block_delta", {"index": 0, "delta": delta}),
        ("content_block_stop", {"index": 0}),
        ("message_delta", {"delta": {"stop_reason": stop, "stop_sequence": None}, "usage": {"output_tokens": 1}}),
        ("message_stop", {}),
    ]
    return "text/event-stream", "".join(
        f"event: {name}\ndata: {json.dumps({'type': name, **data})}\n\n" for name, data in events)


def openai(body):
    call = pick(body, openai=True)
    if call:
        tool = {"id": call["id"], "type": "function",
                "function": {"name": call["name"], "arguments": json.dumps(call["input"])}}
        message, stop = {"role": "assistant", "content": None, "tool_calls": [tool]}, "tool_calls"
        delta = {"role": "assistant", "tool_calls": [{"index": 0, **tool}]}
    else:
        message, stop = {"role": "assistant", "content": "done"}, "stop"
        delta = {"role": "assistant", "content": "done"}
    head = {"id": "chatcmpl-alarms", "created": 0, "model": body.get("model", "m")}
    usage = {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    if not body.get("stream"):
        return "application/json", json.dumps({**head, "object": "chat.completion", "usage": usage,
                                               "choices": [{"index": 0, "message": message, "finish_reason": stop}]})
    chunks = [
        {"choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
        {"choices": [{"index": 0, "delta": {}, "finish_reason": stop}]},
        {"choices": [], "usage": usage},
    ]
    return "text/event-stream", "".join(
        f"data: {json.dumps({**head, 'object': 'chat.completion.chunk', **chunk})}\n\n" for chunk in chunks
    ) + "data: [DONE]\n\n"


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def send(self, status, kind, data):
        data = data.encode()
        self.send_response(status)
        self.send_header("content-type", kind)
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        self.send(200, "application/json", "{}")

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))) or b"{}")
        if "count_tokens" in self.path:
            return self.send(200, "application/json", '{"input_tokens": 10}')
        self.send(200, *(openai(body) if "chat/completions" in self.path else anthropic(body)))


# Not http.server's HTTPServer: its server_bind calls socket.getfqdn, a PTR
# lookup that macOS gates behind the Local Network prompt (ADE-120).
server = ThreadingTCPServer(("127.0.0.1", 0), Handler)
Path(sys.argv[1]).write_text(str(server.server_address[1]))
server.serve_forever()
