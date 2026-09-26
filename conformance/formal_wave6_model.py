#!/usr/bin/env python3
from itertools import permutations
deps={"plan":set(),"build":{"plan"},"verify":{"build"}}
valid=[]
for order in permutations(deps):
  done=set(); ok=True
  for task in order:
    if not deps[task]<=done: ok=False; break
    done.add(task)
  if ok: valid.append(order)
assert valid==[("plan","build","verify")], "dependency order violated or ambiguous"
print("coordinator dependency DAG proof: ok")
