#!/usr/bin/env python3
"""Module dependency graph for src/: which module files reference which crate modules.
Usage: python3 -I .untangle/deps.py > .untangle/out/deps.txt"""
import pathlib, re, collections, sys
SRC = pathlib.Path("src")
def modpath(p):
    rel = p.relative_to(SRC).with_suffix("")
    parts = list(rel.parts)
    if parts[-1] in ("mod", "lib"): parts = parts[:-1]
    if parts and parts[0] == "bin": return None
    return "::".join(parts) or "crate"
mods = {}
for p in SRC.rglob("*.rs"):
    m = modpath(p)
    if m is not None: mods[m] = p
known = set(mods)
def resolve(path, here):
    segs = path.split("::")
    if segs[0] == "crate": segs = segs[1:]
    elif segs[0] in ("super", "self"):
        base = here.split("::") if here != "crate" else []
        while segs and segs[0] in ("super", "self"):
            if segs[0] == "super": base = base[:-1]
            segs = segs[1:]
        segs = base + segs
    elif segs[0] in ("levitate_recipe",): segs = segs[1:]
    else: return None
    # longest known prefix
    for k in range(len(segs), 0, -1):
        cand = "::".join(segs[:k])
        if cand in known: return cand
    return None
edges = collections.defaultdict(set)
files = [p for p in SRC.rglob("*.rs")]
for p in files:
    here = modpath(p) or ("bin::" + p.stem)
    text = p.read_text()
    text = re.sub(r'//.*', '', text)
    for m in re.finditer(r'\b((?:crate|super|self|levitate_recipe)(?:::[A-Za-z_][A-Za-z0-9_]*)+)', text):
        t = resolve(m.group(1), here if not here.startswith("bin::") else "crate")
        if t and t != here and not t.startswith(here + "::") and not (here != "crate" and t == "crate"):
            edges[here].add(t)
    for m in re.finditer(r'use\s+(?:crate|super|levitate_recipe)::\{([^;]*)\};', text, re.S):
        pass
def top(m, d=2): return "::".join(m.split("::")[:d])
fanin = collections.Counter(t for s in edges for t in edges[s])
print("## Fan-in (modules most imported)")
for m, c in fanin.most_common(30): print(f"{c:3d}  {m}")
print("\n## Fan-out")
for s in sorted(edges, key=lambda s: -len(edges[s]))[:30]: print(f"{len(edges[s]):3d}  {s}")
print("\n## Edges")
for s in sorted(edges): print(f"{s} -> {', '.join(sorted(edges[s]))}")
# area-level graph and cycles
area = collections.defaultdict(set)
for s in edges:
    for t in edges[s]:
        a, b = top(s, 1 if not s.startswith(('core','helpers','inputs','worker','llm')) else 2), top(t, 1 if not t.startswith(('core','helpers','inputs','worker','llm')) else 2)
        if a != b: area[a].add(b)
print("\n## Area graph")
for a in sorted(area): print(f"{a} -> {', '.join(sorted(area[a]))}")
# SCCs on module graph
idx, low, st, on, sccs, c = {}, {}, [], set(), [], [0]
sys.setrecursionlimit(10000)
def dfs(v):
    idx[v] = low[v] = c[0]; c[0] += 1; st.append(v); on.add(v)
    for w in edges.get(v, ()):
        if w not in idx: dfs(w); low[v] = min(low[v], low[w])
        elif w in on: low[v] = min(low[v], idx[w])
    if low[v] == idx[v]:
        comp = []
        while True:
            w = st.pop(); on.discard(w); comp.append(w)
            if w == v: break
        if len(comp) > 1: sccs.append(comp)
for v in list(edges):
    if v not in idx: dfs(v)
print("\n## Module cycles (SCCs)")
for comp in sccs: print(f"{len(comp)}: {', '.join(sorted(comp))}")
