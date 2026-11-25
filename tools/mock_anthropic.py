#!/usr/bin/env python3
"""Tiny fake of the Anthropic Messages streaming API, for testing the harness
without a key. Point the harness at it with PURRNEL_API_URL=http://127.0.0.1:8999/v1/messages.

Turn 1 (plain user text): streams a spoken sentence, then a create_artefact tool call.
Turn 2 (tool_result):     streams a final spoken sentence.
Text containing 'quiet' gets a [silent] reply; 'erase' triggers purge_artefact('timer'); 'sleep' go_to_sleep; 'scratch' act; 'purr' emote; 'weather' a hosted web_search turn; 'pause' a pause_turn. Request bodies are logged to /tmp/purrnel-mock.jsonl.
"""
import json, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HTML = ("<!doctype html><html><head><meta charset='utf-8'><title>Timer</title></head>"
        "<body style='margin:0;display:grid;place-items:center;height:100vh;background:#162;color:#fff;font:700 18vh system-ui'>"
        "<div id=t>0</div><script>(async()=>{let n=(await purrnel.get('n'))||0;const t=document.getElementById('t');"
        "setInterval(()=>{n++;t.textContent=n;purrnel.set('n',n)},1000)})()</script></body></html>")

def sse(w, ev, data):
    w.write(f"event: {ev}\ndata: {json.dumps(data)}\n\n".encode()); w.flush()

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["content-length"])))
        open("/tmp/purrnel-mock.jsonl", "a").write(json.dumps({"headers": dict(self.headers), "body": body}) + "\n")
        last = body["messages"][-1]
        if last["role"] == "assistant":   # a paused turn handed straight back
            self.send_response(200); self.send_header("content-type", "text/event-stream"); self.end_headers()
            sse(self.wfile, "message_start", {"type": "message_start", "message": {"id": "msg_9", "role": "assistant", "content": []}})
            self.text(self.wfile, 0, ["Right, I found it. ", "It is sunny and twenty degrees."]); self.end(self.wfile, "end_turn"); return
        self.send_response(200); self.send_header("content-type", "text/event-stream"); self.end_headers()
        w = self.wfile
        sse(w, "message_start", {"type": "message_start", "message": {"id": "msg_1", "role": "assistant", "content": []}})
        if isinstance(last["content"], str):
            if "quiet" in last["content"]:
                self.text(w, 0, ["[sil", "ent]"]); self.end(w, "end_turn"); return
            def tool(idx, tid, name, args):
                sse(w, "content_block_start", {"type": "content_block_start", "index": idx,
                    "content_block": {"type": "tool_use", "id": tid, "name": name, "input": {}}})
                sse(w, "content_block_delta", {"type": "content_block_delta", "index": idx,
                    "delta": {"type": "input_json_delta", "partial_json": json.dumps(args)}})
                sse(w, "content_block_stop", {"type": "content_block_stop", "index": idx})
            if "weather" in last["content"] or "pause" in last["content"]:
                pause = "pause" in last["content"]
                self.text(w, 0, ["Let me look that up."])
                sse(w, "content_block_start", {"type": "content_block_start", "index": 1,
                    "content_block": {"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": {}}})
                args = json.dumps({"query": "weather today"})
                for i in range(0, len(args), 7):
                    sse(w, "content_block_delta", {"type": "content_block_delta", "index": 1,
                        "delta": {"type": "input_json_delta", "partial_json": args[i:i+7]}})
                sse(w, "content_block_stop", {"type": "content_block_stop", "index": 1})
                if pause:
                    self.end(w, "pause_turn"); return
                sse(w, "content_block_start", {"type": "content_block_start", "index": 2,
                    "content_block": {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_1",
                                      "content": [{"type": "web_search_result", "url": "https://example.com/w", "title": "Weather", "encrypted_content": "abc", "page_age": None}]}})
                sse(w, "content_block_stop", {"type": "content_block_stop", "index": 2})
                sse(w, "content_block_start", {"type": "content_block_start", "index": 3,
                    "content_block": {"type": "text", "text": "", "citations": None}})
                sse(w, "content_block_delta", {"type": "content_block_delta", "index": 3,
                    "delta": {"type": "text_delta", "text": "It's sunny and twenty degrees today."}})
                sse(w, "content_block_delta", {"type": "content_block_delta", "index": 3,
                    "delta": {"type": "citations_delta", "citation": {"type": "web_search_result_location", "url": "https://example.com/w", "title": "Weather", "encrypted_index": "xyz", "cited_text": "sunny"}}})
                sse(w, "content_block_stop", {"type": "content_block_stop", "index": 3})
                self.end(w, "end_turn"); return
            if "sleep" in last["content"]:
                self.text(w, 0, ["Night night, ", "sleep tight."])
                tool(1, "toolu_4", "go_to_sleep", {"place": "sofa"})
                self.end(w, "tool_use"); return
            if "scratch" in last["content"]:
                self.text(w, 0, ["Ooh, ", "sofa time!"])
                tool(1, "toolu_5", "act", {"actions": [{"do": "scratch", "target": "sofa"}, {"do": "groom"}]})
                self.end(w, "tool_use"); return
            if "purr" in last["content"]:
                self.text(w, 0, ["Mrow! ", "Purr purr. ", "I love that."])
                sse(w, "content_block_start", {"type": "content_block_start", "index": 1,
                    "content_block": {"type": "tool_use", "id": "toolu_3", "name": "emote", "input": {}}})
                sse(w, "content_block_delta", {"type": "content_block_delta", "index": 1,
                    "delta": {"type": "input_json_delta", "partial_json": "{\"name\": \"purr\"}"}})
                sse(w, "content_block_stop", {"type": "content_block_stop", "index": 1})
                self.end(w, "tool_use"); return
            if "erase" in last["content"]:
                self.text(w, 0, ["Okay, ", "erasing it now."])
                sse(w, "content_block_start", {"type": "content_block_start", "index": 1,
                    "content_block": {"type": "tool_use", "id": "toolu_2", "name": "purge_artefact", "input": {}}})
                sse(w, "content_block_delta", {"type": "content_block_delta", "index": 1,
                    "delta": {"type": "input_json_delta", "partial_json": "{\"id\": \"timer\"}"}})
                sse(w, "content_block_stop", {"type": "content_block_stop", "index": 1})
                self.end(w, "tool_use"); return
            self.text(w, 0, ["Sure thing, friend! ", "Let me make you a ", "little timer. ", "One second."])
            sse(w, "content_block_start", {"type": "content_block_start", "index": 1,
                "content_block": {"type": "tool_use", "id": "toolu_1", "name": "create_artefact", "input": {}}})
            args = json.dumps({"title": "Timer", "html": HTML})
            for i in range(0, len(args), 40):
                sse(w, "content_block_delta", {"type": "content_block_delta", "index": 1,
                    "delta": {"type": "input_json_delta", "partial_json": args[i:i+40]}})
            sse(w, "content_block_stop", {"type": "content_block_stop", "index": 1})
            self.end(w, "tool_use")
        else:
            self.text(w, 0, ["All done, it's on the screen. ", "Mrow!"]); self.end(w, "end_turn")
    def text(self, w, idx, parts):
        sse(w, "content_block_start", {"type": "content_block_start", "index": idx, "content_block": {"type": "text", "text": ""}})
        for p in parts:
            sse(w, "content_block_delta", {"type": "content_block_delta", "index": idx, "delta": {"type": "text_delta", "text": p}})
        sse(w, "content_block_stop", {"type": "content_block_stop", "index": idx})
    def end(self, w, reason):
        sse(w, "message_delta", {"type": "message_delta", "delta": {"stop_reason": reason}, "usage": {"output_tokens": 10}})
        sse(w, "message_stop", {"type": "message_stop"})

if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1]) if len(sys.argv) > 1 else 8999), H).serve_forever()
