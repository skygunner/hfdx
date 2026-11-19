# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# How to run: python scripts/qa/fixture.py --port 0 --ready-file PATH --log PATH
# noqa: SIZE_OK - Exact five-file assignment keeps the standalone wire fixture in one file.
"""Local-only Hugging Face HTTP fixture; never use real credentials here."""
from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import re
import signal
import threading
import time
from contextlib import closing
from dataclasses import dataclass
from email.utils import format_datetime, parsedate_to_datetime
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Final, Literal, TypeAlias
from urllib.parse import SplitResult, parse_qs, quote, unquote, urlsplit

JSON: TypeAlias = str | int | float | bool | None | list['JSON'] | dict[str, 'JSON']
COMMITS: Final = {'a': 'a' * 40, 'b': 'b' * 40}
SIZES: Final = {'small-complete.bin': 256 * 1024, 'large-partial.bin': 32 * 1024 * 1024,
                'empty.bin': 0, 'special/space # 中文.bin': 1024}
RANGE: Final = re.compile(r'bytes=(\d*)-(\d*)\Z')
PAGE_SIZE: Final = 1000
BENCHMARK_LIMIT: Final = 1024 ** 3
BENCHMARK_SHA256: Final = '2c06ade942ee3f17a048dd1064b2fab046a4bb95386d8bb41b68dc6711ac2af3'


def payload(offset: int, count: int, commit: str) -> bytes:
    shift = int(commit == COMMITS['b'])
    return bytes((i + offset + shift) % 256 for i in range(256)) * (count // 256) + bytes(
        (i + offset + shift) % 256 for i in range(count % 256))


@dataclass(frozen=True, slots=True)
class Fault:
    request: int
    action: Literal['429', '500', 'close']
    retry_after: int | str = 1


def valid_retry_after(value: JSON) -> bool:
    if type(value) is int:
        return value >= 0
    if not isinstance(value, str) or len(value) != 29:
        return False
    try:
        return format_datetime(parsedate_to_datetime(value), usegmt=True) == value
    except (ValueError, TypeError, OverflowError):
        return False


def benchmark_digest(size: int) -> str:
    if size == BENCHMARK_LIMIT:
        return BENCHMARK_SHA256
    digest = hashlib.sha256()
    chunk = bytes(range(256)) * 4096
    blocks, tail = divmod(size, len(chunk))
    for _ in range(blocks):
        digest.update(chunk)
    digest.update(chunk[:tail])
    return digest.hexdigest()


class State:
    """Mutable request counters and event journal, serialized by one condition."""
    def __init__(self, log: Path, rate: int) -> None:
        self.log = log
        self.rate = rate
        self.branch = 'a'
        self.faults: list[Fault] = []
        self.page2_requests = 0
        self.head_status = 200
        self.download_mode = 'direct'
        self.bearer_marker: str | None = None
        self.tree_mode = 'normal'
        self.hashes: dict[tuple[str, str], str] = {}
        self.link_mode = 'same-origin'
        self.api_status = 200
        self.metadata_redirect = 'none'
        self.catalog_count: int | None = None
        self.benchmark_bytes: int | None = None
        self.benchmark: dict[str, JSON] | None = None
        self.events: list[dict[str, JSON]] = []
        self.condition = threading.Condition()
        self.stop = threading.Event()
        self.origins: dict[str, str] = {}

    def record(self, event: dict[str, JSON]) -> int:
        with self.condition:
            seq = len(self.events) + 1
            event = dict(event, seq=seq, time=time.time())
            event.setdefault('request_id', seq)
            with self.log.open('a', encoding='utf-8') as stream:
                stream.write(json.dumps(event, ensure_ascii=True) + '\n')
            self.events.append(event)
            self.condition.notify_all()
            return seq

    def control(self, value: JSON) -> bool:
        """Parse the entire control before changing any shared state."""
        if not isinstance(value, dict) or value.keys() - {'branch', 'rate', 'faults', 'head_status',
                'download_mode', 'bearer_marker', 'tree_mode', 'link_mode', 'api_status', 'metadata_redirect', 'catalog_count', 'benchmark_bytes'}:
            return False
        branch, rate = value.get('branch', self.branch), value.get('rate', self.rate)
        if branch not in ('a', 'b') or type(rate) is not int or rate < 0:
            return False
        head_status = value.get('head_status', self.head_status)
        api_status = value.get('api_status', self.api_status)
        download_mode = value.get('download_mode', self.download_mode)
        marker = value.get('bearer_marker', self.bearer_marker)
        tree_mode = value.get('tree_mode', self.tree_mode)
        link_mode = value.get('link_mode', self.link_mode)
        catalog_count = value.get('catalog_count', self.catalog_count)
        if catalog_count is not None and (type(catalog_count) is not int or not 1 <= catalog_count <= 1000000):
            return False
        benchmark_bytes = value.get('benchmark_bytes', self.benchmark_bytes)
        if benchmark_bytes is not None and (type(benchmark_bytes) is not int or not 1 <= benchmark_bytes <= BENCHMARK_LIMIT):
            return False
        if benchmark_bytes is not None and catalog_count is not None:
            return False
        metadata_redirect = value.get('metadata_redirect', self.metadata_redirect)
        if metadata_redirect not in ('none', 'same-origin', 'cross-origin', 'loop', 'missing-location'):
            return False
        if type(head_status) is not int or head_status not in (200, 405, 501):
            return False
        if type(api_status) is not int or api_status not in (200, 401, 403, 429):
            return False
        if download_mode not in ('direct', 'bearer', 'redirect'):
            return False
        if 'bearer_marker' in value and (not isinstance(marker, str)
                or not re.fullmatch(r'[A-Za-z0-9_-]{1,128}', marker)):
            return False
        if download_mode == 'bearer' and marker is None:
            return False
        if tree_mode not in ('normal', 'duplicate', 'conflict', 'empty', 'empty-terminal', 'no-time', 'unsafe'):
            return False
        if link_mode not in ('same-origin', 'cross-origin'):
            return False
        faults: list[Fault] = []
        raw = value.get('faults', [])
        if not isinstance(raw, list):
            return False
        for item in raw:
            if not isinstance(item, dict) or item.keys() - {'request', 'action', 'retry_after'}:
                return False
            request, action, retry = item.get('request'), item.get('action'), item.get('retry_after', 1)
            if type(request) is not int or request < 1 or not valid_retry_after(retry):
                return False
            if action not in ('429', '500', 'close') or request in [f.request for f in faults]:
                return False
            match action:
                case '429': faults.append(Fault(request, '429', retry))
                case '500': faults.append(Fault(request, '500', retry))
                case 'close': faults.append(Fault(request, 'close', retry))
        benchmark = self.benchmark
        if benchmark_bytes != self.benchmark_bytes:
            benchmark = None if benchmark_bytes is None else {'size': benchmark_bytes, 'sha256': benchmark_digest(benchmark_bytes)}
        with self.condition:
            self.branch, self.rate = str(branch), rate
            self.head_status, self.api_status = head_status, api_status
            self.download_mode, self.bearer_marker = download_mode, marker
            self.tree_mode, self.link_mode = tree_mode, link_mode
            self.metadata_redirect = metadata_redirect
            self.catalog_count = catalog_count
            self.benchmark_bytes, self.benchmark = benchmark_bytes, benchmark
            if 'faults' in value:
                self.faults, self.page2_requests = faults, 0
            self.condition.notify_all()
        return True


class Server(ThreadingHTTPServer):
    daemon_threads = False
    state: State
    role: str

    def get_request(self):
        connection, address = super().get_request()
        connection.settimeout(2)
        return connection, address


class Handler(BaseHTTPRequestHandler):
    server: Server
    protocol_version = 'HTTP/1.1'

    def log_message(self, format: str, *args: str) -> None:
        """Raw request lines may contain secrets; only the journal is used."""

    def reply(self, status: int, body: bytes = b'', **headers: str) -> None:
        self.status = status
        self.send_response(status)
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Connection', 'close')
        for key, value in headers.items():
            self.send_header(key.replace('_', '-'), value)
        self.end_headers()
        self.close_connection = True
        if self.command != 'HEAD' and body:
            self.wfile.write(body)
            self.sent += len(body)

    def emit(self, kind: str) -> int:
        return self.server.state.record(dict(self.event, kind=kind, bytes=self.sent, status=self.status))

    def do_HEAD(self) -> None:
        self.do_GET()

    def do_POST(self) -> None:
        self.do_GET()

    def do_GET(self) -> None:
        state = self.server.state
        parsed = urlsplit(self.path)
        path = unquote(parsed.path)
        self.sent, self.status = 0, 0
        if path in ('/__state', '/__control', '/__shutdown') and not parsed.scheme:
            self.management(path, parse_qs(parsed.query))
            return
        raw_range = self.headers.get('Range', '')
        self.event: dict[str, JSON] = {'method': self.command if self.command in ('GET', 'HEAD', 'POST') else '[invalid]', 'path': path,
            'range': raw_range if RANGE.fullmatch(raw_range) else ('[invalid]' if raw_range else ''),
            'role': self.server.role, 'auth_present': bool(self.headers.get('Authorization')),
            'cookie_present': bool(self.headers.get('Cookie')),
            'proxy_auth_present': bool(self.headers.get('Proxy-Authorization'))}
        request_id = self.emit('start')
        self.event['request_id'] = request_id
        try:
            if self.server.role == 'proxy':
                self.proxy(parsed)
            else:
                self.route(path, parse_qs(parsed.query))
        except (BrokenPipeError, ConnectionResetError, TimeoutError, http.client.HTTPException):
            self.event['interrupted'] = True
        finally:
            self.emit('complete')

    def management(self, path: str, query: dict[str, list[str]]) -> None:
        state = self.server.state
        if path == '/__shutdown':
            if self.command != 'POST':
                self.reply(405)
                return
            self.reply(202, b'{}', Content_Type='application/json')
            state.stop.set()
            with state.condition:
                state.condition.notify_all()
            return
        if path == '/__control':
            try:
                length = int(self.headers.get('Content-Length', '0'))
                if self.command != 'POST' or not 0 < length <= 65536:
                    self.reply(400)
                    return
                value = json.loads(self.rfile.read(length))
                with state.condition:
                    accepted = state.control(value)
                self.reply(200 if accepted else 400, b'{}')
            except (ValueError, UnicodeDecodeError):
                self.reply(400)
            return
        try:
            after = int(query.get('after', ['0'])[0])
            timeout = min(10.0, max(0.0, float(query.get('timeout', ['0'])[0])))
            if not 0 <= timeout <= 10:
                raise ValueError
        except ValueError:
            self.reply(400)
            return
        def matches(event: dict[str, JSON]) -> bool:
            return (int(str(event['seq'])) > after
                    and ('kind' not in query or event['kind'] == query['kind'][0])
                    and ('path' not in query or event['path'] == query['path'][0])
                    and ('role' not in query or event['role'] == query['role'][0]))
        with state.condition:
            state.condition.wait_for(lambda: state.stop.is_set() or any(map(matches, state.events)), timeout)
            body = {'events': [e for e in state.events if matches(e)], 'branch': state.branch,
                    'rate': state.rate, 'page2_requests': state.page2_requests,
                    'head_status': state.head_status, 'download_mode': state.download_mode,
                    'tree_mode': state.tree_mode, 'link_mode': state.link_mode, 'api_status': state.api_status,
                    'metadata_redirect': state.metadata_redirect, 'catalog_count': state.catalog_count,
                    'catalog_size': state.catalog_count if state.catalog_count is not None else len(SIZES) + int(state.benchmark_bytes is not None),
                    'page_size': PAGE_SIZE, 'benchmark_bytes': state.benchmark_bytes, 'benchmark': state.benchmark}
        self.reply(200, json.dumps(body).encode(), Content_Type='application/json')

    def proxy(self, target: SplitResult) -> None:
        state = self.server.state
        allowed = [urlsplit(url).netloc for role, url in state.origins.items() if role != 'proxy']
        if target.scheme != 'http' or target.netloc not in allowed or self.command not in ('GET', 'HEAD'):
            self.reply(403)
            return
        self.event['destination'] = f'http://{target.netloc}'
        headers = {key: self.headers[key] for key in ('Authorization', 'Range') if key in self.headers}
        with closing(http.client.HTTPConnection('127.0.0.1', target.port, timeout=2)) as connection:
            connection.request(self.command, target.path + ('?' + target.query if target.query else ''), headers=headers)
            response = connection.getresponse()
            self.status = response.status
            self.send_response(response.status)
            for key, value in response.getheaders():
                if key.lower() not in ('connection', 'server', 'date', 'transfer-encoding'):
                    self.send_header(key, value)
            self.send_header('Connection', 'close')
            self.end_headers()
            self.close_connection = True
            while chunk := response.read(16384):
                self.wfile.write(chunk)
                self.sent += len(chunk)

    def route(self, path: str, query: dict[str, list[str]]) -> None:
        if self.command not in ('GET', 'HEAD'):
            self.reply(405)
            return
        state = self.server.state
        if path == '/org/model.git/info/refs':
            def packet(data: bytes) -> bytes:
                return f'{len(data) + 4:04x}'.encode() + data
            commit = COMMITS[state.branch]
            body = packet(b'# service=git-upload-pack\n') + b'0000'
            body += packet(f'{commit} HEAD\0symref=HEAD:refs/heads/master\n'.encode())
            body += packet(f'{commit} refs/heads/master\n'.encode()) + b'0000'
            self.reply(200, body, Content_Type='application/x-git-upload-pack-advertisement')
            return
        if path.startswith('/api/v1/models/org/model/repo'):
            commit = query.get('Revision', [''])[0]
            if commit not in COMMITS.values():
                self.reply(404)
                return
            if state.api_status != 200:
                self.reply(200, json.dumps({'Code': state.api_status, 'Success': False, 'Message': 'fake-secret'}).encode())
                return
            if path.endswith('/files'):
                root = query.get('Root', [''])[0]
                names = [n for n in SIZES if n.rpartition('/')[0] == root]
                entries = [{'Path': name, 'Type': 'blob', 'Size': SIZES[name],
                            'Sha256': hashlib.sha256(payload(0, SIZES[name], commit)).hexdigest(), 'CommittedDate': 1793491200}
                           for name in names]
                if not root:
                    entries.append({'Path': 'special', 'Type': 'tree'})
                if state.tree_mode == 'unsafe':
                    entries[0]['Path'] = '../escape'
                self.reply(200, json.dumps({'Code': 200, 'Success': True, 'Data': {'Files': entries}}).encode(), Content_Type='application/json')
            else:
                name = query.get('FilePath', [''])[0]
                if name not in SIZES:
                    self.reply(404)
                else:
                    self.reply(302, Location=f'{state.origins["capture"]}/org/model/resolve/{commit}/{quote(name, safe="/")}')
            return
        api = re.fullmatch(r'/api/(?:models|datasets)/(.+)/(revision|tree)/([^/]+)', path)
        if api:
            repo, operation, revision = api.groups()
            commit = COMMITS[state.branch] if revision == 'main' else revision
            if commit not in COMMITS.values():
                self.reply(404)
                return
            if state.api_status != 200:
                self.reply(state.api_status)
                return
            if self.server.role == 'origin' and repo == 'org/alias' and state.metadata_redirect != 'none':
                if state.metadata_redirect == 'missing-location':
                    self.reply(307)
                    return
                target_role = 'capture' if state.metadata_redirect == 'cross-origin' else 'origin'
                target_path = path if state.metadata_redirect == 'loop' else path.replace('/org/alias/', '/org/model/', 1)
                raw_query = urlsplit(self.path).query
                location = state.origins[target_role] + quote(target_path, safe='/')
                if raw_query:
                    location += '?' + raw_query
                self.reply(307, Location=location)
                return
            if operation == 'revision':
                self.reply(200, json.dumps({'sha': commit}).encode(), Content_Type='application/json')
                return
            if state.catalog_count is not None:
                cursor = query.get('cursor', ['0'])
                if len(cursor) != 1 or not re.fullmatch(r'[0-9]{1,7}', cursor[0]):
                    self.reply(400)
                    return
                start = int(cursor[0])
                if start > state.catalog_count:
                    self.reply(400)
                    return
                end = min(start + PAGE_SIZE, state.catalog_count)
                entries = [{'type': 'file', 'path': f'perf/path-{index:06d}.bin', 'size': 0}
                           for index in range(start, end)]
                headers = {} if end == state.catalog_count else {'Link':
                    f'<{state.origins["origin"]}{quote(path, safe="/")}?recursive=true&cursor={end}>; rel="next"'}
                self.reply(200, json.dumps(entries).encode(), Content_Type='application/json', **headers)
                return
            if state.tree_mode == 'empty':
                self.reply(200, b'[]', Content_Type='application/json')
                return
            page2 = query.get('cursor') == ['page2']
            if page2:
                with state.condition:
                    state.page2_requests += 1
                    fault = next((f for f in state.faults if f.request == state.page2_requests), None)
                if fault:
                    if fault.action == 'close':
                        self.close_connection = True
                        return
                    self.reply(int(fault.action), Retry_After=str(fault.retry_after))
                    return
            names = list(SIZES)[2:] if page2 else list(SIZES)[:2]
            if page2 and state.tree_mode == 'empty-terminal':
                names = []
            entries: list[dict[str, JSON]] = [{'type': 'file', 'path': name, 'size': SIZES[name]} for name in names]
            for entry in entries:
                if query.get('expand') == ['true'] and state.tree_mode != 'no-time':
                    entry['lastCommit'] = {'date': '2026-11-01T00:00:00Z'}
                digest = state.hashes.get((commit, entry['path']))
                if digest:
                    entry['lfs'] = {'oid': digest, 'size': entry['size']}
            if page2 and state.benchmark_bytes is not None and state.tree_mode != 'empty-terminal':
                entries.append({'type': 'file', 'path': 'benchmark.bin', 'size': state.benchmark_bytes})
            if page2 and state.tree_mode in ('duplicate', 'conflict'):
                entries.insert(0, {'type': 'file', 'path': 'small-complete.bin',
                    'size': SIZES['small-complete.bin'] + int(state.tree_mode == 'conflict')})
            if not page2:
                entries.append({'type': 'directory', 'path': 'special', 'size': 0})
            next_origin = state.origins['capture' if state.link_mode == 'cross-origin' else 'origin']
            headers = {} if page2 else {'Link': f'<{next_origin}/api/{"datasets" if path.startswith("/api/datasets/") else "models"}/{quote(repo, safe="/")}/tree/{commit}?recursive=true&cursor=page2>; rel="next"'}
            if query.get('expand') == ['true'] and 'Link' in headers:
                headers['Link'] = headers['Link'].replace('?recursive=true&cursor=', '?recursive=false&expand=true&cursor=')
            self.reply(200, json.dumps(entries).encode(), Content_Type='application/json', **headers)
            return
        redirect = re.fullmatch(r'/redirect/([^/]+)/(.+)', path)
        if redirect:
            commit, name = redirect.groups()
            self.reply(302, Location=f'{state.origins["capture"]}/org/model/resolve/{commit}/{quote(name, safe="/")}')
            return
        resolved = re.fullmatch(r'/(?:datasets/)?(.+)/resolve/([^/]+)/(.+)', path)
        if not resolved:
            self.reply(404)
            return
        _, revision, name = resolved.groups()
        commit = COMMITS[state.branch] if revision == 'main' else revision
        synthetic = re.fullmatch(r'perf/path-([0-9]{6})\.bin', name)
        is_synthetic = synthetic is not None and state.catalog_count is not None and int(synthetic[1]) < state.catalog_count
        is_benchmark = name == 'benchmark.bin' and state.benchmark_bytes is not None
        if commit not in COMMITS.values() or (name not in SIZES and not is_synthetic and not is_benchmark):
            self.reply(404)
            return
        if self.server.role == 'origin':
            if state.download_mode == 'bearer' and self.headers.get('Authorization') != f'Bearer {state.bearer_marker}':
                self.reply(401)
                return
            if self.command == 'HEAD' and state.head_status != 200:
                self.reply(state.head_status)
                return
            if state.download_mode == 'redirect':
                self.reply(302, Location=state.origins['capture'] + quote(path, safe='/'))
                return
        self.file(name, commit)

    def file(self, name: str, commit: str) -> None:
        state = self.server.state
        size = state.benchmark_bytes if name == 'benchmark.bin' else SIZES.get(name, 0)
        start, end = 0, size - 1
        status = 200
        headers = {'Accept-Ranges': 'bytes', 'Content-Type': 'application/octet-stream', 'ETag': f'"{commit}"'}
        raw = self.headers.get('Range') if self.command == 'GET' else None
        if raw:
            match = RANGE.fullmatch(raw)
            if not match or not any(match.groups()) or not size:
                self.reply(416, Content_Range=f'bytes */{size}')
                return
            left, right = match.groups()
            start = int(left) if left else max(0, size - int(right))
            end = min(size - 1, int(right)) if left and right else size - 1
            if start >= size or end < start or (not left and int(right) == 0):
                self.reply(416, Content_Range=f'bytes */{size}')
                return
            status, headers['Content-Range'] = 206, f'bytes {start}-{end}/{size}'
        self.status = status
        self.send_response(status)
        for key, value in dict(headers, **{'Content-Length': str(end - start + 1), 'Connection': 'close'}).items():
            self.send_header(key, value)
        self.end_headers()
        self.close_connection = True
        if self.command == 'HEAD':
            return
        began = time.monotonic()
        while start <= end and not state.stop.is_set():
            rate = state.rate
            if rate and state.stop.wait(max(0, began + self.sent / rate - time.monotonic())):
                break
            chunk = payload(start, min(16384, end - start + 1), COMMITS['a'] if name == 'benchmark.bin' else commit)
            self.wfile.write(chunk)
            self.wfile.flush()
            self.sent += len(chunk)
            start += len(chunk)
            self.emit('progress')
        if start <= end:
            self.event['interrupted'] = True


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', type=int, default=0)
    parser.add_argument('--ready-file', type=Path, required=True)
    parser.add_argument('--log', type=Path, required=True)
    parser.add_argument('--rate', type=int, default=262144, help='bytes/second per request; 0 unlimited')
    parser.add_argument('--benchmark-bytes', type=int, default=None, help='opt-in benchmark.bin size, 1..1073741824')
    parser.add_argument('--hashes', action='store_true', help='publish real SHA-256 for deterministic content verification QA')
    args = parser.parse_args()
    if not 0 <= args.port <= 65535 or args.rate < 0:
        parser.error('port must be 0..65535 and rate nonnegative')
    state = State(args.log, args.rate)
    if not state.control({'benchmark_bytes': args.benchmark_bytes}):
        parser.error('benchmark-bytes must be 1..1073741824')
    servers: list[Server] = []
    threads: list[threading.Thread] = []
    def stop(signum: int, frame) -> None:
        state.stop.set()
        with state.condition:
            state.condition.notify_all()
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    try:
        for role in ('origin', 'capture', 'proxy'):
            server = Server(('127.0.0.1', args.port if role == 'origin' else 0), Handler)
            server.state, server.role = state, role
            servers.append(server)
            state.origins[role] = f'http://127.0.0.1:{server.server_port}'
            thread = threading.Thread(target=server.serve_forever, name=role)
            threads.append(thread)
            thread.start()
        fixtures = {}
        for name, size in SIZES.items():
            digest = hashlib.sha256()
            for offset in range(0, size, 65536):
                digest.update(payload(offset, min(65536, size - offset), COMMITS['a']))
            fixtures[name] = {'size': size, 'sha256': digest.hexdigest()}
        if args.hashes:
            for commit in COMMITS.values():
                for name, size in SIZES.items():
                    digest = hashlib.sha256()
                    for offset in range(0, size, 65536):
                        digest.update(payload(offset, min(65536, size - offset), commit))
                    state.hashes[(commit, name)] = digest.hexdigest()
        if state.benchmark is not None:
            fixtures['benchmark.bin'] = state.benchmark
        ready = dict(state.origins, port=servers[0].server_port, commits=COMMITS, fixtures=fixtures,
                     catalog_count=state.catalog_count, catalog_size=len(SIZES) + int(state.benchmark_bytes is not None),
                     page_size=PAGE_SIZE, benchmark_bytes=state.benchmark_bytes, benchmark=state.benchmark)
        args.ready_file.write_text(json.dumps(ready) + '\n', encoding='utf-8')
        print(json.dumps(ready), flush=True)
        state.stop.wait()
    finally:
        state.stop.set()
        with state.condition:
            state.condition.notify_all()
        for server in servers:
            server.shutdown()
            server.server_close()
        for thread in threads:
            thread.join()
        print(json.dumps({'cleanup': 'listeners closed; request threads joined'}), flush=True)


if __name__ == '__main__':
    main()
