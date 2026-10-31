"""Copy Zed crates (and their Zed-internal dependencies) into vendor/zed/.

Usage (from the hermes-rs root, needs `cargo` on PATH):
    uv run --no-project --with tomlkit python -I scripts/vendor_zed.py ../zed gpui gpui_platform ui theme

What it does:
1. Runs `cargo metadata` in the Zed checkout and walks the dependency graph from the given root
   crates, following only path (Zed-internal) dependencies that apply on Windows.
2. Copies each crate directory into vendor/zed/<name> (replacing any previous copy).
3. Rewrites each copied Cargo.toml so it only refers to crates that were copied: dependency
   entries, target-specific sections and feature entries naming anything else are removed. Cargo
   requires every path dependency to exist, even ones for other platforms, so this is necessary.
4. Regenerates the block between the `# BEGIN vendored-zed` / `# END vendored-zed` markers in the
   root Cargo.toml's [workspace.dependencies] with exactly the entries the copied crates use,
   taken from Zed's root Cargo.toml. Keys already declared outside the block are left alone.
5. Records the Zed commit and the crate list in vendor/SOURCES.md.

Re-running it is safe: it is how we re-sync with a newer Zed or add more crates.
"""

import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

import tomlkit

ROOT = Path(__file__).resolve().parent.parent
VENDOR = ROOT / "vendor" / "zed"
BEGIN, END = "# BEGIN vendored-zed", "# END vendored-zed"
# Crates we never copy even if something depends on them on Windows.
EXCLUDED = {"assets"}
# Optional third-party dependencies we remove from copied manifests (with the features that
# enable them): `scap` is screen capture for Zed's collaboration features, fetched from git.
DROPPED_DEPS = {"scap"}


def eval_cfg(expr: str) -> bool:
    """Evaluate a cargo `cfg(...)` expression for x86_64-pc-windows-msvc."""
    facts = {
        "target_os": "windows", "target_family": "windows", "target_arch": "x86_64",
        "target_env": "msvc", "target_vendor": "pc", "target_pointer_width": "64",
    }
    flags = {"windows"}
    tokens = re.findall(r'[A-Za-z_][A-Za-z0-9_]*|"[^"]*"|[(),=]', expr)
    pos = 0

    def parse():
        nonlocal pos
        name = tokens[pos]; pos += 1
        if name in ("any", "all", "not"):
            pos += 1  # (
            values = []
            while tokens[pos] != ")":
                values.append(parse())
                if tokens[pos] == ",":
                    pos += 1
            pos += 1  # )
            if name == "any":
                return any(values)
            if name == "all":
                return all(values)
            return not values[0]
        if pos < len(tokens) and tokens[pos] == "=":
            value = tokens[pos + 1].strip('"'); pos += 2
            return facts.get(name) == value
        return name in flags

    if expr.startswith("cfg("):
        expr = expr[4:-1]
        tokens = re.findall(r'[A-Za-z_][A-Za-z0-9_]*|"[^"]*"|[(),=]', expr)
        return parse()
    return expr == "x86_64-pc-windows-msvc"


def closure(meta: dict, roots: list[str]) -> list[str]:
    packages = {p["name"]: p for p in meta["packages"]}
    seen, stack = set(), list(roots)
    while stack:
        name = stack.pop()
        if name in seen or name in EXCLUDED:
            continue
        seen.add(name)
        for dep in packages[name]["dependencies"]:
            if not dep.get("path") or dep["kind"] == "dev" or dep["name"] not in packages:
                continue
            if dep.get("target") and not eval_cfg(dep["target"]):
                continue
            stack.append(dep["name"])
    return sorted(seen)


def to_lf(directory: Path) -> None:
    """Convert CRLF to LF in copied text files (a Windows Git checkout of Zed has CRLF)."""
    for path in directory.rglob("*"):
        if path.is_file():
            data = path.read_bytes()
            if b"\0" not in data[:8000] and b"\r\n" in data:
                path.write_bytes(data.replace(b"\r\n", b"\n"))


def inline(value):
    """Convert a parsed TOML value into an inline form that fits on one `key = value` line."""
    if isinstance(value, dict):
        table = tomlkit.inline_table()
        table.update({k: inline(v) for k, v in value.items()})
        return table
    if isinstance(value, list):
        array = tomlkit.array()
        array.extend(inline(v) for v in value)
        return array
    return tomlkit.item(value)


def dep_name(key: str, value) -> str:
    if isinstance(value, dict) and "package" in value:
        return value["package"]
    return key


def is_internal(name: str, zed_ws_deps: dict) -> bool:
    entry = zed_ws_deps.get(name)
    return isinstance(entry, dict) and "path" in entry


def prune_manifest(path: Path, keep: set[str], zed_ws_deps: dict) -> set[str]:
    """Drop references to Zed crates that weren't copied. Returns the workspace deps still used."""
    doc = tomlkit.parse(path.read_text(encoding="utf-8"))
    removed: set[str] = set()
    used: set[str] = set()

    def prune_table(table):
        for key in list(table.keys()):
            value = table[key]
            name = dep_name(key, value)
            internal = is_internal(name, zed_ws_deps) or (isinstance(value, dict) and "path" in value)
            if (internal and name not in keep) or key in DROPPED_DEPS:
                removed.add(key)
                del table[key]
            elif isinstance(value, dict) and value.get("workspace"):
                used.add(name)

    for section in ("dependencies", "dev-dependencies", "build-dependencies"):
        if section in doc:
            prune_table(doc[section])
    if "target" in doc:
        for cfg in list(doc["target"].keys()):
            if not eval_cfg(cfg):
                del doc["target"][cfg]
                continue
            for section in ("dependencies", "dev-dependencies", "build-dependencies"):
                if section in doc["target"][cfg]:
                    prune_table(doc["target"][cfg][section])
    if "features" in doc:
        # A feature entry is "other_feature", "dep:name", "name/feature" or "name?/feature". Keep it
        # only if what it names still exists after removing other platforms' sections.
        remaining = set(doc["features"].keys())
        for section in ("dependencies", "build-dependencies"):
            remaining |= set(doc.get(section, {}).keys())
        for cfg in doc.get("target", {}).values():
            for section in ("dependencies", "build-dependencies"):
                remaining |= set(cfg.get(section, {}).keys())
        for feature, entries in doc["features"].items():
            kept = [e for e in entries if e.split("/")[0].removeprefix("dep:").rstrip("?") in remaining]
            entries.clear()
            entries.extend(kept)
    path.write_text(tomlkit.dumps(doc), encoding="utf-8", newline="\n")
    return used


def main() -> None:
    zed = Path(sys.argv[1]).resolve()
    roots = sys.argv[2:]
    meta = json.loads(subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=zed, capture_output=True, text=True, check=True).stdout)
    zed_root = tomlkit.parse((zed / "Cargo.toml").read_text(encoding="utf-8"))
    zed_ws_deps = zed_root["workspace"]["dependencies"]

    existing = {p.name for p in VENDOR.iterdir()} if VENDOR.exists() else set()
    crates = sorted(set(closure(meta, roots)) | existing)
    packages = {p["name"]: p for p in meta["packages"]}

    used: set[str] = set()
    for name in crates:
        src = Path(packages[name]["manifest_path"]).parent
        dst = VENDOR / name
        if dst.exists():
            shutil.rmtree(dst)
        shutil.copytree(src, dst, ignore=shutil.ignore_patterns("target", ".DS_Store"))
        to_lf(dst)
        used |= prune_manifest(dst / "Cargo.toml", set(crates), zed_ws_deps)

    cargo_toml = ROOT / "Cargo.toml"
    text = cargo_toml.read_text(encoding="utf-8", newline="\n")
    before, rest = text.split(BEGIN, 1)
    _, after = rest.split(END, 1)
    declared_outside = set(tomlkit.parse(before + after)["workspace"]["dependencies"].keys())
    lines = []
    for key in sorted(used):
        if key in declared_outside:
            continue
        if key in crates:
            lines.append(f'{key} = {{ path = "vendor/zed/{key}" }}')
        elif key in zed_ws_deps:
            lines.append(f"{key} = {inline(zed_ws_deps[key].unwrap()).as_string().strip()}")
    cargo_toml.write_text(f"{before}{BEGIN}\n" + "\n".join(lines) + f"\n{END}{after}", encoding="utf-8", newline="\n")

    commit = subprocess.run(["git", "rev-parse", "HEAD"], cwd=zed, capture_output=True, text=True,
                            check=True).stdout.strip()
    sources = ROOT / "vendor" / "SOURCES.md"
    body = sources.read_text(encoding="utf-8") if sources.exists() else ""
    body = re.sub(r"<!-- BEGIN zed -->.*<!-- END zed -->", "", body, flags=re.S).rstrip()
    body += (f"\n\n<!-- BEGIN zed -->\n## Zed\n\nCommit `{commit}` from https://github.com/zed-industries/zed, "
             f"copied by `scripts/vendor_zed.py`.\n\nCrates ({len(crates)}): "
             + ", ".join(f"`{c}`" for c in crates)
             + "\n\nAssets: `assets/fonts`, `assets/icons`, `assets/themes` copied from Zed's `assets/`."
             + "\n<!-- END zed -->\n")
    sources.write_text(body.lstrip() + "\n", encoding="utf-8", newline="\n")
    print(f"vendored {len(crates)} crates, {len(lines)} workspace dependency entries")


if __name__ == "__main__":
    main()
