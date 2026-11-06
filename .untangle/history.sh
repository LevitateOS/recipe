#!/usr/bin/env bash
# Churn and co-change for the recipe crate. Usage: bash .untangle/history.sh > .untangle/out/history.txt
set -u
since="${SINCE:-1 year ago}"
echo "## Churn (commits touching file, since $since)"
git log --since="$since" --name-only --pretty=format: -- src tests | grep -v '^$' | sort | uniq -c | sort -rn | head -60
echo
echo "## Co-change pairs (src files, commits with <= 12 src files)"
git log --since="$since" --name-only --pretty=format:'@@' -- src | python3 -I -c '
import sys, itertools, collections
commits, cur = [], []
for line in sys.stdin:
    line = line.strip()
    if line == "@@":
        if cur: commits.append(cur)
        cur = []
    elif line: cur.append(line)
if cur: commits.append(cur)
pairs = collections.Counter(); single = collections.Counter()
for files in commits:
    files = sorted(set(f for f in files if f.startswith("src/")))
    for f in files: single[f] += 1
    if len(files) > 12: continue
    for a, b in itertools.combinations(files, 2): pairs[(a, b)] += 1
def area(f): return "/".join(f.split("/")[:3]) if f.count("/") > 1 else f
print("count  jaccard  pair  [far = different top-level area]")
rows = []
for (a, b), c in pairs.items():
    if c < 3: continue
    j = c / (single[a] + single[b] - c)
    far = a.split("/")[1] != b.split("/")[1]
    rows.append((c, j, a, b, far))
for c, j, a, b, far in sorted(rows, key=lambda r: (-r[0], -r[1]))[:80]:
    print(f"{c:4d}  {j:.2f}  {a}  <->  {b}{"  [far]" if far else ""}")
'
