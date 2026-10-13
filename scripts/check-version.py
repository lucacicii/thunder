#!/usr/bin/env python3
"""One version for the whole workspace — and a way to prove it.

Thunder ships four host-consumed artifacts built from ten crates. A release is
identified by a `vX.Y.Z` tag, and the binaries report the version they were
compiled with (`CARGO_PKG_VERSION`, i.e. `[workspace.package].version`). Those
two facts have to agree, or an artifact exists in the world that claims one
version and answers with another — and the first question when a protocol
mismatch shows up is "which build is this?".

So: any change to any subpackage bumps the single workspace version, and this
script is what refuses the release if it did not happen. It is deliberately
the *only* implementation of that rule, and both `ci.yml` and `release.yml`
run it, so a fix in one place cannot drift from the other.

What it checks:

  1. the workspace version, from the root `Cargo.toml` (`[workspace.package]`);
  2. every member crate inherits it (`version.workspace = true`) instead of
     hardcoding its own — a hardcoded literal is how crates drift apart;
  3. `Cargo.lock` records every member at that version (a stale lock would
     also break the release's `--locked` build, one step later and less
     clearly);
  4. `docs/architecture.md` names the current version — the doc is what a
     human reads before touching the tree;
  5. with `--tag`, the releasing tag matches it. This is the release gate.

Deliberately NOT checked, because they are different versions on purpose:
plugin manifest versions (`src/plugins/*.rs`), the NDJSON `PROTOCOL_VERSION`,
`rust-version`/edition, and third-party dependency versions.

Usage:
    python3 scripts/check-version.py
    python3 scripts/check-version.py --tag v0.5.0
"""

import argparse
import os
import re
import sys

CRATES_EXPECTED_NOTE = "every crate in `[workspace] members`"


def die(problems):
    """Report every problem, not just the first: one run should say it all."""
    print("✗ 版本一致性检查未通过：\n", file=sys.stderr)
    for p in problems:
        print("  - {0}".format(p), file=sys.stderr)
    print(
        "\n  修法：改根 Cargo.toml 的 [workspace.package].version（唯一真源），"
        "\n       再 cargo build 让 Cargo.lock 跟上；成员 crate 一律用 version.workspace = true。",
        file=sys.stderr,
    )
    sys.exit(1)


def section(text, name):
    """Body of a TOML table header `[name]`, up to the next table header."""
    m = re.search(r"^\[{0}\]\s*$".format(re.escape(name)), text, re.M)
    if not m:
        return None
    rest = text[m.end():]
    nxt = re.search(r"^\[", rest, re.M)
    return rest[: nxt.start()] if nxt else rest


def key(body, name):
    """First `name = "value"` / `name = value` in a table body."""
    m = re.search(r"^{0}\s*=\s*(.+?)\s*(?:#.*)?$".format(re.escape(name)), body, re.M)
    return m.group(1).strip() if m else None


def unquote(value):
    if value is None:
        return None
    value = value.strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
        return value[1:-1]
    return value


def read(path):
    with open(path, "r", encoding="utf-8") as f:
        return f.read()


def members_of(root_toml_text):
    """The `members = [...]` list of `[workspace]`, in declaration order."""
    ws = section(root_toml_text, "workspace")
    if ws is None:
        return None
    m = re.search(r"^members\s*=\s*\[(.*?)\]", ws, re.M | re.S)
    if not m:
        return None
    return re.findall(r"\"([^\"]+)\"", m.group(1))


def lock_versions(lock_text):
    """`{name: version}` from `Cargo.lock`'s `[[package]]` stanzas."""
    found = {}
    for stanza in lock_text.split("[[package]]")[1:]:
        name = unquote(key(stanza, "name"))
        version = unquote(key(stanza, "version"))
        if name and version:
            found[name] = version
    return found


def main():
    ap = argparse.ArgumentParser(description="Thunder workspace version consistency gate")
    ap.add_argument("--repo-root", default=None, help="defaults to this script's parent directory")
    ap.add_argument(
        "--tag",
        default=None,
        help="the git tag being released (e.g. v0.5.0); must match the workspace version",
    )
    args = ap.parse_args()

    root_dir = args.repo_root or os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    root_toml_path = os.path.join(root_dir, "Cargo.toml")
    problems = []

    root_text = read(root_toml_path)
    ws_pkg = section(root_text, "workspace.package")
    if ws_pkg is None:
        die(["{0} 里没有 [workspace.package] 段".format(root_toml_path)])
    version = unquote(key(ws_pkg, "version"))
    if not version:
        die(["{0} 的 [workspace.package] 里没有 version".format(root_toml_path)])

    members = members_of(root_text)
    if not members:
        die(["{0} 的 [workspace] 里没有 members".format(root_toml_path)])
    if len(members) < 2:
        problems.append(
            "只解析到 {0} 个 member（{1}）—— members 列表大概被改动过，检查解析是否还成立".format(
                len(members), ", ".join(members)
            )
        )

    # 2. Every member inherits the workspace version. The package *name* is what
    # Cargo.lock keys on, and for the nested members (`thunder-agent-core/tui`,
    # `.../conversation`) it is not the directory name — read it, never guess.
    member_pkgs = []
    for member in members:
        manifest = os.path.join(root_dir, member, "Cargo.toml")
        if not os.path.exists(manifest):
            problems.append("member `{0}` 的 Cargo.toml 不存在".format(member))
            continue
        pkg = section(read(manifest), "package")
        if pkg is None:
            problems.append("{0} 里没有 [package] 段".format(manifest))
            continue
        pkg_name = unquote(key(pkg, "name"))
        if pkg_name:
            member_pkgs.append((member, pkg_name))
        else:
            problems.append("{0} 里没有 package name".format(manifest))
        # Inheritance is a dotted key (`version.workspace = true`); the inline
        # table form is accepted too. A literal is how one crate drifts.
        dotted = re.search(r"^version\s*\.\s*workspace\s*=\s*true\s*$", pkg, re.M)
        inline = re.search(r"^version\s*=\s*\{\s*workspace\s*=\s*true\s*\}\s*$", pkg, re.M)
        literal = re.search(r"^version\s*=\s*(.+?)\s*$", pkg, re.M)
        if not (dotted or inline):
            if literal:
                problems.append(
                    "{0} 硬写了 version = {1}；应改为 `version.workspace = true`，"
                    "否则它与 workspace 版本会各走各的".format(manifest, literal.group(1))
                )
            else:
                problems.append(
                    "{0} 没有 version（应写 `version.workspace = true`）".format(manifest)
                )

    # 3. Cargo.lock agrees (a stale lock also breaks the release's --locked build).
    lock_path = os.path.join(root_dir, "Cargo.lock")
    if not os.path.exists(lock_path):
        problems.append("没有 Cargo.lock —— 发版用 `--locked` 构建，必须提交它")
    else:
        locked = lock_versions(read(lock_path))
        for member, name in member_pkgs:
            if name not in locked:
                problems.append(
                    "Cargo.lock 里没有 `{0}`（member {1}）—— 跑一次 cargo build 让它跟上".format(
                        name, member
                    )
                )
            elif locked[name] != version:
                problems.append(
                    "Cargo.lock 里 `{0}` 是 {1}，workspace 是 {2} —— "
                    "跑一次 cargo build 同步（否则发布时 --locked 会直接失败）".format(
                        name, locked[name], version
                    )
                )

    # 4. The doc a human reads before touching the tree names the same version.
    doc_path = os.path.join(root_dir, "docs", "architecture.md")
    if os.path.exists(doc_path):
        m = re.search(r"版本：\s*([0-9]+\.[0-9]+\.[0-9]+)", read(doc_path))
        if not m:
            problems.append("docs/architecture.md 里找不到 `版本：x.y.z`，无法核对")
        elif m.group(1) != version:
            problems.append(
                "docs/architecture.md 写的是 {0}，workspace 是 {1}".format(m.group(1), version)
            )

    # 5. The release gate: the tag and the compiled-in version are the same claim.
    if args.tag:
        expected = "v" + version
        if args.tag != expected:
            problems.append(
                "发布 tag `{0}` 与 workspace 版本对不上（应为 `{1}`）。"
                "只改了某个子包也必须提升版本号，否则产物会自称一个版本、"
                "而二进制应答另一个".format(args.tag, expected)
            )

    if problems:
        die(problems)

    print("✓ 版本一致确认：workspace {0}".format(version))
    print("  · {0} 个 member crate 全部 version.workspace = true".format(len(members)))
    print("  · Cargo.lock 里各 member 均为 {0}".format(version))
    if os.path.exists(doc_path):
        print("  · docs/architecture.md 记录 {0}".format(version))
    if args.tag:
        print("  · 发布 tag {0} 与 workspace 版本一致".format(args.tag))
    return 0


if __name__ == "__main__":
    sys.exit(main())
