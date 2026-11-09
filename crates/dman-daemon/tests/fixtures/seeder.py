"""Test fixture: a local tracker plus an aria2c seeder for a hand-built torrent.

Usage: seeder.py WORKDIR
Prints one JSON line {"magnet", "info_hash", "files": {path: size}} once the
seeder is ready, then runs until stdin is closed. Stdlib only.
"""
import base64, hashlib, http.server, json, os, socket, struct, subprocess, sys, threading, time, urllib.request
from urllib.parse import parse_qs, quote_from_bytes, urlparse


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


def benc(x):
    if isinstance(x, int):
        return b"i%de" % x
    if isinstance(x, str):
        x = x.encode()
    if isinstance(x, bytes):
        return b"%d:%s" % (len(x), x)
    if isinstance(x, list):
        return b"l" + b"".join(benc(i) for i in x) + b"e"
    if isinstance(x, dict):
        items = sorted((k.encode() if isinstance(k, str) else k, v) for k, v in x.items())
        return b"d" + b"".join(benc(k) + benc(v) for k, v in items) + b"e"
    raise TypeError(x)


def main(work):
    os.makedirs(work, exist_ok=True)
    peers = {}

    class H(http.server.BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def do_GET(self):
            q = parse_qs(urlparse(self.path).query, encoding="latin1")
            ih = q["info_hash"][0].encode("latin1").hex()
            port = int(q["port"][0])
            lst = peers.setdefault(ih, {})
            lst[port] = 1
            blob = b"".join(socket.inet_aton("127.0.0.1") + struct.pack(">H", p) for p in lst if p != port)
            body = benc({"interval": 60, "peers": blob})
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    tport = free_port()
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", tport), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    announce = f"http://127.0.0.1:{tport}/announce"

    name = "Album"
    files = {"01-intro.bin": os.urandom(30000), "sub/02-main.bin": os.urandom(50000), "03-end.bin": os.urandom(20000)}
    content = os.path.join(work, "content")
    data = b""
    flist = []
    for rel, b in files.items():
        p = os.path.join(content, name, rel)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        open(p, "wb").write(b)
        data += b
        flist.append({"length": len(b), "path": rel.split("/")})
    pl = 16384
    pieces = b"".join(hashlib.sha1(data[i:i + pl]).digest() for i in range(0, len(data), pl))
    info = {"name": name, "piece length": pl, "pieces": pieces, "files": flist}
    torrent = benc({"announce": announce, "info": info})
    ih = hashlib.sha1(benc(info)).hexdigest()

    rpc = free_port()
    seed = subprocess.Popen(
        ["aria2c", "--enable-rpc", f"--rpc-listen-port={rpc}", "--rpc-secret=t", f"--dir={work}/seeddir",
         "--enable-dht=false", "--enable-dht6=false", "--bt-enable-lpd=false", "--enable-peer-exchange=false",
         f"--listen-port={free_port()}", "--file-allocation=none", "--check-integrity=false",
         "--bt-seed-unverified=true", "--seed-ratio=0.0", "--seed-time=1000", "--summary-interval=0"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def call(method, *params):
        body = {"jsonrpc": "2.0", "id": "x", "method": "aria2." + method, "params": ["token:t", *params]}
        req = urllib.request.Request(f"http://127.0.0.1:{rpc}/jsonrpc", json.dumps(body).encode(),
                                     {"Content-Type": "application/json"})
        return json.load(urllib.request.urlopen(req, timeout=10))["result"]

    try:
        for _ in range(100):
            try:
                call("getVersion")
                break
            except Exception:
                time.sleep(0.1)
        gid = call("addTorrent", base64.b64encode(torrent).decode(), [], {"dir": content})
        for _ in range(100):
            if call("tellStatus", gid, ["status", "seeder"]).get("seeder") == "true":
                break
            time.sleep(0.1)
        magnet = f"magnet:?xt=urn:btih:{ih}&dn={name}&tr={quote_from_bytes(announce.encode())}"
        print(json.dumps({"magnet": magnet, "info_hash": ih,
                          "files": {k: len(v) for k, v in files.items()}, "name": name}), flush=True)
        sys.stdin.read()  # until the test closes our stdin
    finally:
        try:
            call("forceShutdown")
        except Exception:
            pass
        try:
            seed.wait(5)
        except Exception:
            seed.kill()
        srv.shutdown()


main(sys.argv[1])
