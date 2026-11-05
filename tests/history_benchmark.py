#!/usr/bin/env -S uv run
"""Benchmark saved history through a disposable daemon; never start Codex.

Example: uv run tests/history_benchmark.py --database /private/events.sqlite \
    --session UUID --binary target/rust-pwa/release/demodex --verify-pages
The source database is opened read-only; only the selected session's events are
copied. A three-column (seq, at, message) events snapshot is also accepted.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import tempfile
import time


def benchmark(binary, database, session, verify_pages, through=None, exercise_cache=False):
    with tempfile.TemporaryDirectory(prefix="demodex-history-benchmark-") as temporary:
        directory = Path(temporary)
        state = directory / "state"
        state.mkdir(mode=0o700)
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        with (directory / "daemon.log").open("w") as log:
            # No host workspace, VM, runtime profile, attached executor, or
            # queued notifications. PATH cannot resolve a Codex executable.
            def start_daemon():
                return subprocess.Popen(
                    [str(binary), "--api-only", "--bind", f"127.0.0.1:{port}", "--data-dir", str(state)],
                    stdout=log, stderr=log,
                    env=os.environ | {"PATH": "/nonexistent", "CODEX_HOME": str(directory / "unused-profile")},
                )
            process = start_daemon()
            try:
                for _ in range(100):
                    if (state / "access-token").exists():
                        break
                    assert process.poll() is None, "Disposable daemon exited"
                    time.sleep(.1)
                else:
                    raise RuntimeError("Disposable daemon did not start")
                with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as source:
                    columns = {row[1] for row in source.execute("PRAGMA table_info(events)")}
                    query = "SELECT seq,at,message FROM events"
                    parameters = ()
                    if "session_id" in columns:
                        query += " WHERE session_id=?"
                        parameters = (session,)
                    if through is not None:
                        query += (" AND" if parameters else " WHERE") + " seq<=?"
                        parameters += (through,)
                    with sqlite3.connect(state / "state.sqlite") as destination:
                        destination.execute(
                            "INSERT INTO sessions(id,name,endpoint,thread_id,targets,status) VALUES(?,?,?,?,?,?)",
                            (session, "Offline history benchmark", "ws://127.0.0.1:1", "fixture", "[]", "disconnected"),
                        )
                        destination.execute("INSERT INTO session_presentation VALUES(?,?)", (session, json.dumps({"name":"Benchmark", "icon":"", "context_reporting":False, "context":None})))
                        destination.execute("INSERT INTO session_order VALUES(?,0,0)", (session,))
                        destination.executemany(
                            "INSERT INTO events(seq,session_id,at,message) VALUES(?,?,?,?)",
                            ((seq, session, at, message) for seq, at, message in source.execute(query + " ORDER BY seq", parameters)),
                        )

                def call(older=None):
                    started = time.perf_counter()
                    completed = subprocess.run(
                        [str(binary), "call", "--url", f"ws://127.0.0.1:{port}/wormhole",
                         "--token-file", str(state / "access-token")],
                        input=json.dumps({"operation": {"Conversation": {"id": session, "older": older}}}) + "\n",
                        text=True, capture_output=True, timeout=180, check=True,
                    )
                    elapsed = time.perf_counter() - started
                    result = json.loads(completed.stdout)
                    if "Ok" not in result:
                        raise RuntimeError(result)
                    return result["Ok"], elapsed, len(completed.stdout.encode())

                first, elapsed, size = call()
                results = [{"case": "cold_initial", "seconds": elapsed, "response_bytes": size, "items": len(first["items"])}]
                pages = [first["items"]]
                if first["older"]:
                    older, elapsed, size = call(first["older"])
                    results.append({"case": "cached_older", "seconds": elapsed, "response_bytes": size, "items": len(older["items"])})
                    pages.insert(0, older["items"])
                    while verify_pages and older["older"]:
                        older, _, _ = call(older["older"])
                        assert older["cursor"] == first["cursor"]
                        pages.insert(0, older["items"])
                repeated, elapsed, size = call()
                assert repeated == first, "Cached first page changed without new events"
                results.append({"case": "cached_initial", "seconds": elapsed, "response_bytes": size, "items": len(repeated["items"])})
                if exercise_cache:
                    process.terminate()
                    process.wait(timeout=20)
                    process = start_daemon()
                    for _ in range(1800):
                        assert process.poll() is None, "Disposable daemon exited on restart: " + (directory / "daemon.log").read_text()[-4000:]
                        try:
                            with socket.create_connection(("127.0.0.1", port), timeout=.1):
                                break
                        except OSError:
                            time.sleep(.1)
                    else:
                        raise RuntimeError("Disposable daemon did not restart")
                    restored, elapsed, size = call()
                    assert restored == first, "Checkpoint changed the first page"
                    results.append({"case": "after_restart", "seconds": elapsed, "response_bytes": size, "items": len(restored["items"])})
                    with sqlite3.connect(state / "state.sqlite") as destination:
                        destination.execute("INSERT INTO events(session_id,message) VALUES(?,?)", (session, '{"method":"future/benchmark"}'))
                    updated, elapsed, size = call()
                    assert updated["items"] == first["items"]
                    assert updated["event_count"] == first["event_count"] + 1
                    assert updated["cursor"] > first["cursor"]
                    results.append({"case": "one_new_event", "seconds": elapsed, "response_bytes": size, "items": len(updated["items"])})
                digest = hashlib.sha256()
                ids = set()
                for page in pages:
                    for item in page:
                        assert item["id"] not in ids, "Duplicate paged item"
                        ids.add(item["id"])
                        digest.update(json.dumps(item, ensure_ascii=False, separators=(",", ":"), sort_keys=True).encode())
                        digest.update(b"\n")
                children = []
                for task in Path(f"/proc/{process.pid}/task").iterdir():
                    children.extend((task / "children").read_text().split())
                assert not children, "Disposable daemon started a child process"
                assert not (directory / "unused-profile").exists(), "A runtime profile was created"
                indexed = None
                with sqlite3.connect(state / "state.sqlite") as destination:
                    if destination.execute("SELECT 1 FROM sqlite_master WHERE name='conversation_index'").fetchone():
                        indexed = destination.execute("SELECT version,through,event_count,item_count FROM conversation_index WHERE session_id=?", (session,)).fetchone()
                return {"binary": str(binary), "events": first["event_count"], "cursor": first["cursor"], "index_state": indexed,
                        "reads": results, "verified_all_pages": verify_pages, "verified_items": len(ids),
                        "canonical_items_sha256": digest.hexdigest()}
            finally:
                process.terminate()
                try:
                    process.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/rust-pwa/release/demodex"))
    parser.add_argument("--database", type=Path, required=True)
    parser.add_argument("--session", required=True)
    parser.add_argument("--verify-pages", action="store_true")
    parser.add_argument("--exercise-cache", action="store_true", help="Also measure daemon restart and an incremental event")
    parser.add_argument("--through", type=int, help="Pin the event high-water mark for before/after comparisons")
    parser.add_argument("--repeat", type=int, default=1)
    args = parser.parse_args()
    for _ in range(args.repeat):
        print(json.dumps(benchmark(args.binary.resolve(), args.database.resolve(), args.session, args.verify_pages, args.through, args.exercise_cache)), flush=True)
