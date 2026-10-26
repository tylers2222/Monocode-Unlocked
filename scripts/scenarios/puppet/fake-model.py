#!/usr/bin/env python3
"""A local Messages API for scripts/scenarios/app.sh d4: fake-model.py <port-file> <probes.json>

Writes its port to <port-file>. A request whose messages contain "D4-HABIT"
gets probe n as a tool call (n = tool results so far), then text; probes.json
is app.sh's list (fields `name` and `input` are used). Others get text.
"""
import json
import sys
from pathlib import Path
from http.server import BaseHTTPRequestHandler
from socketserver import ThreadingTCPServer

PROBES = json.loads(Path(sys.argv[2]).read_text())


def reply(body):
    text = json.dumps(body.get("messages", []))
    tools = {tool["name"] for tool in body.get("tools", [])}
    n = text.count('"tool_result"')
    if "D4-HABIT" in text and n < len(PROBES) and PROBES[n]["name"] in tools:
        probe = PROBES[n]
        return {"type": "tool_use", "id": f"toolu_d4_{n}", "name": probe["name"], "input": probe["input"]}, "tool_use"
    return {"type": "text", "text": "done"}, "end_turn"


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def send(self, status, kind, data):
        self.send_response(status)
        self.send_header("content-type", kind)
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        self.send(200, "application/json", b"{}")

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))) or b"{}")
        if "count_tokens" in self.path:
            return self.send(200, "application/json", b'{"input_tokens": 10}')
        block, stop = reply(body)
        usage = {"input_tokens": 1, "output_tokens": 1}
        message = {"id": "msg_d4", "type": "message", "role": "assistant", "model": body.get("model", "m"),
                   "stop_sequence": None, "usage": usage}
        if not body.get("stream"):
            return self.send(200, "application/json",
                             json.dumps({**message, "content": [block], "stop_reason": stop}).encode())
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
        self.send(200, "text/event-stream",
                  "".join(f"event: {name}\ndata: {json.dumps({'type': name, **data})}\n\n" for name, data in events).encode())


# Not http.server's HTTPServer: its server_bind calls socket.getfqdn, a PTR
# lookup through mDNSResponder that macOS gates behind the Local Network
# prompt, which blocks this server on a hosted runner (ADE-120).
server = ThreadingTCPServer(("127.0.0.1", 0), Handler)
Path(sys.argv[1]).write_text(str(server.server_address[1]))
server.serve_forever()
