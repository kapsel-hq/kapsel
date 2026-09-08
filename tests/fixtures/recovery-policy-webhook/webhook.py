#!/usr/bin/env python3
"""Disposable receiver evidence, with explicit bounded admission barriers.

The control port is fixture-only, exposed through the owned cluster's Service
proxy. Never install this deliberately side-effecting webhook in an existing cluster.
"""

import base64
import http.server
import json
import ssl
import threading

MAX_BODY_BYTES = 1 << 20
OPERATION_ANNOTATION = "kapsel.dev/kap0038-operation-id"
CONDITION = threading.Condition()
SCENARIOS = {}


class Handler(http.server.BaseHTTPRequestHandler):
    def read_json(self):
        length = int(self.headers.get("content-length", "0"))
        if length <= 0 or length > MAX_BODY_BYTES:
            raise ValueError("invalid body length")
        return json.loads(self.rfile.read(length))

    def respond(self, body):
        encoded = json.dumps(body, separators=(",", ":")).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    def log_message(self, _format, *_args):
        return


class ControlHandler(Handler):
    def do_POST(self):  # noqa: N802 - stdlib handler API
        command = self.read_json()
        operation_id = command["operation_id"]
        with CONDITION:
            if command["action"] == "configure":
                if operation_id in SCENARIOS or len(SCENARIOS) >= 32:
                    self.send_error(409)
                    return
                SCENARIOS[operation_id] = {
                    "hold": command.get("hold", False),
                    "invalidate_first": command.get("invalidate_first", False),
                    "released": 0,
                    "invocations": [],
                    "effects": [],
                }
            state = SCENARIOS[operation_id]
            if command["action"] == "release":
                state["released"] = command["through"]
                CONDITION.notify_all()
            if command["action"] == "allow":
                state["invalidate_first"] = False
            self.respond(state)


class AdmissionHandler(Handler):
    def do_POST(self):  # noqa: N802 - stdlib handler API
        review = self.read_json()
        request = review["request"]
        annotations = request.get("object", {}).get("metadata", {}).get("annotations", {})
        operation_id = annotations.get(OPERATION_ANNOTATION, "missing")
        response = {"uid": request["uid"], "allowed": True}
        with CONDITION:
            state = SCENARIOS.get(operation_id)
            ordinal = 0
            if state is not None:
                if len(state["invocations"]) >= 16:
                    self.send_error(429)
                    return
                state["invocations"].append(request["uid"])
                ordinal = len(state["invocations"])
            if not request.get("dryRun", False):
                # The log is the actual out-of-band effect. The separate ledger
                # lets the test cross-check invocations against observed pod logs.
                print(
                    f"KAPSEL_ADMISSION_EFFECT uid={request['uid']} "
                    f"operation_id={operation_id} ordinal={ordinal}",
                    flush=True,
                )
                if state is not None:
                    state["effects"].append(request["uid"])
            if state is not None and state["hold"]:
                released = CONDITION.wait_for(lambda: state["released"] >= ordinal, timeout=20)
                if not released:
                    response["allowed"] = False
                    response["status"] = {"message": "fixture barrier deadline exceeded"}
            if state is not None and state["invalidate_first"]:
                # Keep every invocation invalid until the test receives the first
                # API response and explicitly enables replay. One API request can
                # re-enter admission. Built-in validation rejects before persistence.
                patch = [{"op": "replace", "path": "/spec/replicas", "value": -1}]
                response["patchType"] = "JSONPatch"
                response["patch"] = base64.b64encode(json.dumps(patch).encode()).decode()
        self.respond(
            {
                "apiVersion": "admission.k8s.io/v1",
                "kind": "AdmissionReview",
                "response": response,
            }
        )


if __name__ == "__main__":
    control = http.server.ThreadingHTTPServer(("0.0.0.0", 8080), ControlHandler)
    threading.Thread(target=control.serve_forever, daemon=True).start()
    server = http.server.ThreadingHTTPServer(("0.0.0.0", 8443), AdmissionHandler)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain("/tls/tls.crt", "/tls/tls.key")
    server.socket = context.wrap_socket(server.socket, server_side=True)
    server.serve_forever()
