#!/usr/bin/env python3
import re, sys, importlib.util, collections
spec = importlib.util.spec_from_file_location("fixes", ".untangle/fixes.py"); m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
text = open(sys.argv[1]).read()
all_ids = re.findall(r"^- \*\*([A-Z]\d+)\*\* \[", text, re.M)
seen = collections.Counter()
for f in m.FIXES:
    for i in m.ids(f["ids"]): seen[i] += 1
for i, _ in m.LEAVE: seen[i] += 1
missing = [i for i in all_ids if seen[i] == 0]
dupes = [i for i, c in seen.items() if c > 1]
unknown = [i for i in seen if i not in set(all_ids)]
print("total", len(all_ids), "missing", missing, "dupes", dupes, "unknown", unknown)
for f in m.FIXES: print(f["key"], len(m.ids(f["ids"])), f["title"])
print("LEAVE", len(m.LEAVE))
