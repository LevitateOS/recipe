#!/usr/bin/env python3
"""Move every problem line from ## Problems under its fix in ## Fixes (per .untangle/fixes.py).
Idempotent only on a log whose Problems still hold the lines. Usage: python3 -I .untangle/apply_fixes.py UNTANGLE.md"""
import re, sys, importlib.util, collections
spec = importlib.util.spec_from_file_location("fixes", ".untangle/fixes.py"); m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
p = sys.argv[1]; text = open(p).read()
pre, rest = text.split("\n## Problems\n", 1)
problems, rest = rest.split("\n## Fixes\n", 1)
fixes_old, progress = rest.split("\n## Progress", 1)
already_moved = "## Fixes" in text and re.search(r"^- \*\*[A-Z]\d+\*\* \[", fixes_old, re.M) is not None
preamble = open(".untangle/fixes_preamble.md").read().rstrip()
lines = problems.split("\n")
if already_moved:
    lines = fixes_old.split("\n")
entry = {}
kinds = collections.Counter()
for ln in lines:
    mm = re.match(r"- \*\*([A-Z]\d+)\*\* \[(\w+)\]", ln)
    if mm: entry[mm.group(1)] = ln; kinds[mm.group(2)] += 1
# Test-quality block moves under X16.
tq = re.search(r"\n#### Test quality.*?(?=\n### )", fixes_old if already_moved else problems, re.S)
tq_text = tq.group(0).strip() if tq else ""
kept = []
skip_tq = False
for ln in ([] if already_moved else lines):
    if ln.startswith("#### Test quality"): skip_tq = True; continue
    if skip_tq and ln.startswith("### "): skip_tq = False
    if skip_tq: continue
    if re.match(r"- \*\*[A-Z]\d+\*\* \[", ln): continue
    kept.append(ln)
kept_text = "\n".join(kept) if not already_moved else problems
kept_text = re.sub(r"\n{3,}", "\n\n", kept_text)
kept_text = re.sub(r"^\(counts pending merge\)\n", "", kept_text.lstrip("\n"))
counts = (f"**{sum(kinds.values())} problems**: {kinds['hidden']} hidden, {kinds['needless']} needless, "
          f"{kinds['surplus']} surplus, **{kinds['special']} special cases** (the number the refactor works to bring down).\n\n"
          "Every entry has been moved under its fix in `## Fixes` (or under `### Leave as is`), keeping its ID. "
          "The section headings below record where each ID range came from.\n")
if already_moved:
    out = [pre, "\n## Problems\n", problems.rstrip("\n"), "\n\n## Fixes\n\n", preamble, "\n\n"]
else:
    out = [pre, "\n## Problems\n\n", counts, "\n", kept_text.strip(), "\n\n## Fixes\n\n", preamble, "\n\n"]
for n, f in enumerate(m.FIXES, 1):
    fids = m.ids(f["ids"])
    k = collections.Counter(re.match(r"- \*\*\w+\*\* \[(\w+)\]", entry[i]).group(1) for i in fids)
    out.append(f"### {n}. {f['key']}: {f['title']}\n\n")
    out.append(f"{f['summary']}\n\n")
    out.append(f"- **Solves:** {len(fids)} problems ({k['hidden']} hidden, {k['needless']} needless, {k['surplus']} surplus, {k['special']} special)\n")
    out.append(f"- **Done when:** {f['done']}\n")
    out.append(f"- **Depends on:** {f['depends']}\n\n")
    for i in fids: out.append(entry[i] + "\n")
    if f["key"] == "X16" and tq_text:
        out.append("\n" + tq_text.replace("#### Test quality (input for the Tests addition)", "#### Test quality: worklist for the Tests addition") + "\n")
    out.append("\n")
out.append("### Leave as is\n\n")
for i, why in m.LEAVE:
    out.append(entry[i] + "\n  - **Why leave:** " + why + "\n")
out.append("\n## Progress" + progress)
open(p, "w").write("".join(out))
print("moved", len(entry), dict(kinds))
