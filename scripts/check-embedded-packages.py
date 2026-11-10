import http.server, json, re, subprocess, sys, threading

types = re.findall(r'^\s*\(Model::\w+, "([^"]+)"', open(sys.argv[1]).read(), re.M)
device = {}

class Stock(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        answers = {
            "/shelly": {"type": device["type"], "mac": "AABBCCDDEEFF", "auth": False, "fw": "smoke"},
            "/status": {"wifi_sta": {"rssi": -50}},
        }
        body = json.dumps(answers.get(self.path, {})).encode()
        self.send_response(200 if self.path in answers else 404)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass

server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Stock)
threading.Thread(target=server.serve_forever, daemon=True).start()
host = f"127.0.0.1:{server.server_port}"
failed = []
for device_type in types:
    device["type"] = device_type
    out = subprocess.run([sys.argv[2], "probe", host],
                         capture_output=True, text=True, timeout=30)
    ok = out.returncode == 0 and "the migration package is built into this program" in out.stdout
    print(f"{device_type}: {'embedded' if ok else 'MISSING'}")
    if not ok:
        print(out.stderr or out.stdout, file=sys.stderr)
        failed.append(device_type)
if not types or failed:
    sys.exit(f"packages missing for {failed or 'all models'}")
