#!/usr/bin/env python3
"""Emit a skeleton of the Rust sources: fn bodies replaced with `{ … }`.
Usage: python3 -I .untangle/skeleton.py [root=src] > .untangle/out/skeleton.rs"""
import sys, pathlib, re

def strip(src: str) -> str:
    out, i, n = [], 0, len(src)
    fn_pending = False  # saw `fn` token at depth where body brace will follow
    while i < n:
        c = src[i]
        # comments
        if src.startswith("//", i):
            j = src.find("\n", i); j = n if j < 0 else j
            line = src[i:j]
            if line.startswith("///") or line.startswith("//!"):
                pass  # drop doc comments too, to save space
            i = j; continue
        if src.startswith("/*", i):
            j = src.find("*/", i + 2); i = n if j < 0 else j + 2; continue
        # raw strings
        m = re.match(r'b?r(#*)"', src[i:i+8])
        if m and (i == 0 or not (src[i-1].isalnum() or src[i-1] == '_')):
            hashes = m.group(1); end = '"' + hashes
            j = src.find(end, i + len(m.group(0)))
            j = n if j < 0 else j + len(end)
            out.append(src[i:j]); i = j; continue
        if c == '"':
            j = i + 1
            while j < n and src[j] != '"':
                j += 2 if src[j] == '\\' else 1
            out.append(src[i:j+1]); i = j + 1; continue
        if c == "'":
            m = re.match(r"'(\\.|[^\\'])'", src[i:i+4]) or re.match(r"'\\u\{[0-9a-fA-F]+\}'", src[i:i+12])
            if m:
                out.append(m.group(0)); i += len(m.group(0)); continue
        if re.match(r'\bfn\b', src[i:i+3]) and (i == 0 or not (src[i-1].isalnum() or src[i-1] == '_')):
            fn_pending = True
        if c == ';' and fn_pending:
            fn_pending = False
        if c == '{' and fn_pending:
            # skip balanced body
            depth, j = 0, i
            while j < n:
                if src.startswith("//", j):
                    k = src.find("\n", j); j = n if k < 0 else k; continue
                if src.startswith("/*", j):
                    k = src.find("*/", j + 2); j = n if k < 0 else k + 2; continue
                mm = re.match(r'b?r(#*)"', src[j:j+8])
                if mm and not (src[j-1].isalnum() or src[j-1] == '_'):
                    e = '"' + mm.group(1); k = src.find(e, j + len(mm.group(0))); j = n if k < 0 else k + len(e); continue
                if src[j] == '"':
                    k = j + 1
                    while k < n and src[k] != '"':
                        k += 2 if src[k] == '\\' else 1
                    j = k + 1; continue
                if src[j] == "'":
                    mm = re.match(r"'(\\.|[^\\'])'", src[j:j+4]) or re.match(r"'\\u\{[0-9a-fA-F]+\}'", src[j:j+12])
                    if mm: j += len(mm.group(0)); continue
                if src[j] == '{': depth += 1
                elif src[j] == '}':
                    depth -= 1
                    if depth == 0: j += 1; break
                j += 1
            out.append("{ … }"); i = j; fn_pending = False; continue
        out.append(c); i += 1
    text = "".join(out)
    text = re.sub(r'\n\s*\n+', '\n', text)
    return text

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "src")
for p in sorted(root.rglob("*.rs")):
    print(f"// ===== {p} =====")
    print(strip(p.read_text()))
