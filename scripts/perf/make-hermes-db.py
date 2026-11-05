"""Builds a Hermes home with Hermes's own SessionDB (from target/hermes-src), for checking the
session importer: two sessions in the default profile (one with a tool call and its result, one
with an interrupted tool call) and one in a `work` profile.

    uv run --no-project --python 3.12 --with openai --with certifi --with truststore \
        --with python-dotenv --with fire --with httpx --with rich --with tenacity \
        --with tomli-w --with ruamel.yaml python scripts/perf/make-hermes-db.py <home> <folder>
"""

import json
import os
import sys
from pathlib import Path

home, folder = Path(sys.argv[1]), sys.argv[2]
os.environ["HERMES_HOME"] = str(home)
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "target" / "hermes-src"))

from hermes_state import SessionDB  # noqa: E402


def build(db_path: Path, sessions) -> None:
    db_path.parent.mkdir(parents=True, exist_ok=True)
    db = SessionDB(db_path=db_path)
    for sid, title, messages in sessions:
        db.create_session(sid, "cli", model="some/model", cwd=folder)
        if title:
            db.set_session_title(sid, title)
        for message in messages:
            db.append_message(sid, **message)
    db.close()


calls = [{"id": "call_1", "type": "function", "function": {"name": "read_file", "arguments": json.dumps({"path": "notes.md"})}}]
build(home / "state.db", [
    ("s_tools", "Read the notes", [
        {"role": "user", "content": "what do my notes say?"},
        {"role": "assistant", "content": "", "tool_calls": calls},
        {"role": "tool", "content": "Buy milk.", "tool_call_id": "call_1", "tool_name": "read_file"},
        {"role": "assistant", "content": "Your notes say: buy milk."},
    ]),
    ("s_cut", None, [
        {"role": "user", "content": "list the folder please"},
        {"role": "assistant", "content": "Listing.", "tool_calls": [
            {"id": "call_9", "type": "function", "function": {"name": "terminal", "arguments": "{\"command\": \"ls\"}"}}]},
    ]),
])
build(home / "profiles" / "work" / "state.db", [
    ("w_1", "Work chat", [
        {"role": "user", "content": "hello from work"},
        {"role": "assistant", "content": "Hi, work."},
    ]),
])
print("built", home)
