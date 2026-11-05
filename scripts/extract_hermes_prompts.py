"""Copy prompt text and tool schemas out of a Hermes checkout into prompts/hermes/.

Usage (from the hermes-rs root):
    uv run --no-project python -I scripts/extract_hermes_prompts.py ../hermes-agent

This never imports or runs Hermes. It parses the source files with `ast` and evaluates
module-level constants with a tiny interpreter that understands literals, f-strings, `+`,
dict/list/tuple literals, `x if c else y`, comparisons, a few string methods, `from x import y`
between Hermes modules, and calls to Hermes functions whose bodies only assign, branch and return.
Anything else is skipped and listed in prompts/hermes/SOURCE.md.

Output: `prompts/hermes/<module>/<NAME>.txt` for strings of 80+ characters, `.json` for values
that look like tool schemas. Platform-dependent values are fixed to Windows (see OVERRIDES).
"""

import ast
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "prompts" / "hermes"

FILES = [
    "agent/prompt_builder.py",
    "agent/background_review.py",
    "agent/curator.py",
    "agent/learn_prompt.py",
    "agent/plan_prompt.py",
    "agent/review_engine.py",
    "hermes_cli/init_command.py",
    "agent/context_compressor.py",
    "agent/title_generator.py",
    "tools/memory_tool.py",
    "tools/memory_tool_store.py",
    "tools/skill_manager_tool.py",
    "tools/skills_tool.py",
    "tools/session_search_tool.py",
    "tools/todo_tool.py",
    "tools/clarify_tool.py",
    "tools/terminal_tool.py",
    "tools/process_registry.py",
    "tools/file_tools.py",
    "tools/web_tools.py",
    "tools/vision_tools.py",
    "tools/delegate_tool.py",
]

# Values that depend on the host platform or environment in Hermes, fixed to what they are on
# Windows with default settings.
OVERRIDES = {
    "_MACOS_SEARCH_NOTE": "",
    "FOREGROUND_MAX_TIMEOUT": 600,
}

CONSTANT_NAME = re.compile(r"_?[A-Z][A-Z0-9_]+")
STRING_METHODS = {"strip", "lstrip", "rstrip", "format", "join", "replace", "upper", "lower"}


class Skip(Exception):
    pass


class Return(Exception):
    def __init__(self, value):
        self.value = value


_MODULES: dict[Path, "Evaluator"] = {}


def module_evaluator(hermes: Path, module: str) -> "Evaluator | None":
    path = hermes / (module.replace(".", "/") + ".py")
    if not path.exists():
        path = hermes / module.replace(".", "/") / "__init__.py"
    if not path.exists():
        return None
    if path not in _MODULES:
        _MODULES[path] = Evaluator(ast.parse(path.read_text(encoding="utf-8")), hermes)
        _MODULES[path].run_module()
    return _MODULES[path]


class Evaluator:
    def __init__(self, tree: ast.Module, hermes: Path):
        self.tree = tree
        self.hermes = hermes
        self.values: dict[str, object] = {}
        self.functions = {n.name: n for n in tree.body if isinstance(n, ast.FunctionDef)}
        self.imports: dict[str, tuple[str, str]] = {}
        for node in ast.walk(tree):
            if isinstance(node, ast.ImportFrom) and node.module and node.level == 0:
                for alias in node.names:
                    self.imports.setdefault(alias.asname or alias.name, (node.module, alias.name))

    def assignments(self):
        """Yield (names, value-or-Skip) for each module-level assignment, recording values."""
        for node in self.tree.body:
            if isinstance(node, ast.Assign):
                targets = node.targets
            elif isinstance(node, ast.AnnAssign) and node.value:
                targets = [node.target]
            else:
                continue
            names = [t.id for t in targets if isinstance(t, ast.Name)]
            if not names:
                continue
            if names[0] in OVERRIDES:
                value = OVERRIDES[names[0]]
            else:
                try:
                    value = self.eval(node.value)
                except Skip as reason:
                    yield names, reason
                    continue
                except Exception as error:
                    yield names, Skip(repr(error))
                    continue
            for name in names:
                self.values[name] = value
            yield names, value

    def run_module(self):
        for _ in self.assignments():
            pass

    def lookup(self, name, scope):
        if name in scope:
            return scope[name]
        if name in OVERRIDES:
            return OVERRIDES[name]
        if name in self.values:
            return self.values[name]
        if name in self.imports:
            module, original = self.imports[name]
            other = module_evaluator(self.hermes, module)
            if other is not None and other is not self:
                return other.lookup(original, {})
        raise Skip(f"unknown name {name}")

    def call(self, fn: ast.FunctionDef, args: list, kwargs: dict):
        params = [a.arg for a in fn.args.args]
        kwonly = [a.arg for a in fn.args.kwonlyargs]
        scope = dict(zip(params[len(params) - len(fn.args.defaults):], (self.eval(d) for d in fn.args.defaults)))
        for arg, default in zip(kwonly, fn.args.kw_defaults):
            if default is not None:
                scope[arg] = self.eval(default)
        scope.update(zip(params, args))
        extra = {k: v for k, v in kwargs.items() if k not in params and k not in kwonly}
        scope.update({k: v for k, v in kwargs.items() if k not in extra})
        if fn.args.kwarg:
            scope[fn.args.kwarg.arg] = extra
        try:
            self.exec_block(fn.body, scope)
        except Return as result:
            return result.value
        return None

    def exec_block(self, body, scope):
        for stmt in body:
            match stmt:
                case ast.Expr(value=ast.Constant()) | ast.Pass():
                    continue
                case ast.Return(value=value):
                    raise Return(self.eval(value, scope) if value else None)
                case ast.Assign(targets=[ast.Name(id=name)], value=value):
                    scope[name] = self.eval(value, scope)
                case ast.AnnAssign(target=ast.Name(id=name), value=value) if value is not None:
                    scope[name] = self.eval(value, scope)
                case ast.If(test=test, body=then, orelse=orelse):
                    self.exec_block(then if self.eval(test, scope) else orelse, scope)
                case _:
                    raise Skip(f"statement {type(stmt).__name__}")

    def eval(self, node, scope=None):
        scope = {} if scope is None else scope
        match node:
            case ast.Constant(value=value):
                return value
            case ast.Name(id=name):
                return self.lookup(name, scope)
            case ast.JoinedStr(values=parts):
                out = []
                for part in parts:
                    if isinstance(part, ast.Constant):
                        out.append(part.value)
                    else:
                        spec = self.eval(part.format_spec, scope) if part.format_spec else ""
                        out.append(format(self.eval(part.value, scope), spec))
                return "".join(out)
            case ast.BinOp(left=left, op=ast.Add(), right=right):
                return self.eval(left, scope) + self.eval(right, scope)
            case ast.IfExp(test=test, body=then, orelse=orelse):
                return self.eval(then if self.eval(test, scope) else orelse, scope)
            case ast.UnaryOp(op=ast.Not(), operand=operand):
                return not self.eval(operand, scope)
            case ast.BoolOp(op=op, values=values):
                result = None
                for value in values:
                    result = self.eval(value, scope)
                    if (isinstance(op, ast.And) and not result) or (isinstance(op, ast.Or) and result):
                        return result
                return result
            case ast.Compare(left=left, ops=[op], comparators=[right]):
                a, b = self.eval(left, scope), self.eval(right, scope)
                if isinstance(op, ast.Eq):
                    return a == b
                if isinstance(op, ast.NotEq):
                    return a != b
                if isinstance(op, ast.Is):
                    return a is b
                if isinstance(op, ast.IsNot):
                    return a is not b
                if isinstance(op, (ast.In, ast.NotIn)):
                    return (a in b) == isinstance(op, ast.In)
                raise Skip("comparison")
            case ast.Dict(keys=keys, values=values):
                out = {}
                for key, value in zip(keys, values):
                    if key is None:
                        out.update(self.eval(value, scope))
                    else:
                        out[self.eval(key, scope)] = self.eval(value, scope)
                return out
            case ast.List(elts=elts) | ast.Tuple(elts=elts):
                out = []
                for elt in elts:
                    if isinstance(elt, ast.Starred):
                        out.extend(self.eval(elt.value, scope))
                    else:
                        out.append(self.eval(elt, scope))
                return out
            case ast.Call(func=ast.Attribute(value=obj, attr=method), args=args, keywords=keywords) if method in STRING_METHODS:
                target = self.eval(obj, scope)
                if not isinstance(target, str):
                    raise Skip("method on non-string")
                return getattr(target, method)(*(self.eval(a, scope) for a in args),
                                               **{k.arg: self.eval(k.value, scope) for k in keywords})
            case ast.Call(func=ast.Name(id="dict"), args=[], keywords=keywords):
                return {k.arg: self.eval(k.value, scope) for k in keywords}
            case ast.Call(func=ast.Name(id=name), args=args, keywords=keywords) if name in self.functions:
                kwargs = {}
                for k in keywords:
                    if k.arg is None:
                        kwargs.update(self.eval(k.value, scope))
                    else:
                        kwargs[k.arg] = self.eval(k.value, scope)
                return self.call(self.functions[name], [self.eval(a, scope) for a in args], kwargs)
        raise Skip(type(node).__name__)


def looks_like_schema(value) -> bool:
    return (isinstance(value, dict) and ("parameters" in value or "properties" in value)
            and "description" in json.dumps(value))


def main() -> None:
    hermes = Path(sys.argv[1]).resolve()
    if OUT.exists():
        shutil.rmtree(OUT)
    written, skipped = 0, []
    for rel in FILES:
        ev = module_evaluator(hermes, Path(rel).with_suffix("").as_posix().replace("/", "."))
        module_dir = OUT / Path(rel).with_suffix("").as_posix().replace("/", ".")
        ev.values.clear()
        for names, value in ev.assignments():
            if isinstance(value, Skip):
                if CONSTANT_NAME.fullmatch(names[0]):
                    skipped.append(f"{rel}:{names[0]} ({value})")
                continue
            for name in names:
                if not CONSTANT_NAME.fullmatch(name):
                    continue
                if isinstance(value, str) and len(value) >= 80:
                    path, text = module_dir / f"{name.lstrip('_')}.txt", value
                elif looks_like_schema(value):
                    path = module_dir / f"{name.lstrip('_')}.json"
                    text = json.dumps(value, indent=2, ensure_ascii=False) + "\n"
                else:
                    continue
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text, encoding="utf-8", newline="\n")
                written += 1

    commit = subprocess.run(["git", "rev-parse", "HEAD"], cwd=hermes, capture_output=True, text=True,
                            check=True).stdout.strip()
    (OUT / "SOURCE.md").write_text(
        f"Extracted by `scripts/extract_hermes_prompts.py` from https://github.com/NousResearch/hermes-agent "
        f"at commit `{commit}`. MIT License, (c) 2025 Nous Research (see `licenses/MIT-hermes-agent.txt`).\n\n"
        "Skipped constants (not statically evaluable):\n\n" + "".join(f"- `{s}`\n" for s in skipped),
        encoding="utf-8", newline="\n")
    print(f"wrote {written} files, skipped {len(skipped)} constants")


if __name__ == "__main__":
    main()
