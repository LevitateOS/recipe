#!/usr/bin/env python3
"""Give every problem line in UNTANGLE.md a stable ID (<section letter><n>), idempotent.
Usage: python3 -I .untangle/number.py UNTANGLE.md"""
import re, sys
p = sys.argv[1]; lines = open(p).read().split("\n")
sec = None; counts = {}; out = []; in_problems = False
for line in lines:
    if line.startswith("## "): in_problems = line.strip() == "## Problems"
    m = re.match(r"### ([A-Z])\. ", line)
    if in_problems and m: sec = m.group(1)
    if in_problems and sec and re.match(r"- \[(hidden|needless|surplus|special)\]", line):
        counts[sec] = counts.get(sec, 0) + 1
        line = f"- **{sec}{counts[sec]}** " + line[2:]
    elif in_problems and sec and re.match(r"- \*\*[A-Z]\d+\*\* ", line):
        counts[sec] = counts.get(sec, 0) + 1
    out.append(line)
open(p, "w").write("\n".join(out))
print(counts, sum(counts.values()))
