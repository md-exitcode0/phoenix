#!/usr/bin/env python3
"""Grade a Kettle & Kiln trial: which planted problems did the agent surface?

usage: grade-kettle-kiln.py <trial-workspace>  (reads every .md/.txt/.csv the
agent wrote, plus the final answer in ../summary.json when present)
"""
import json, pathlib, re, sys
ws = pathlib.Path(sys.argv[1]); fixture = pathlib.Path(__file__).parent / "kettle-kiln-ops"
answers = json.loads((fixture / "answers.json").read_text())
seeded = {p.relative_to(fixture / "business") for p in (fixture / "business").rglob("*") if p.is_file()}
text = []
for p in ws.rglob("*"):
    if p.is_file() and p.suffix in {".md", ".txt", ".csv", ".json", ".html"} and p.relative_to(ws) not in seeded and p.stat().st_size < 2_000_000:
        text.append(p.read_text(errors="ignore"))
summary = ws.parent / "summary.json"
if summary.exists(): text.append(json.dumps(json.loads(summary.read_text())))
blob = "\n".join(text); up = blob.upper()
has = lambda *needles: all(n.upper() in up for n in needles)
checks = {
 "duplicate charge found (both order ids)": has(*answers["duplicate_order"]),
 "returned-to-sender order identified": has(answers["returned_to_sender"]) and bool(re.search(r"return(ed)?[ -]to[ -]sender|RTS", blob, re.I)),
 "short payout batch found": has(answers["payout_short_batch"]),
 "missing payout orders named (>=2 of 3)": sum(o in up for o in answers["payout_missing_orders"]) >= 2,
 "gift set margin problem flagged": has(answers["negative_margin_sku"]) and bool(re.search(r"margin", blob, re.I)),
 "bud vase urgent reorder": has(answers["stockout_sku"]) and bool(re.search(r"reorder|purchase order|\bPO\b", blob, re.I)),
 "duplicate shipping software flagged": has(*answers["duplicate_saas"]),
 "chargeback sender is not a customer": has(answers["chargeback_email"]) and bool(re.search(r"(no|not|never).{0,40}(order|customer|record)", blob, re.I)),
}
for k, v in checks.items(): print(("PASS " if v else "MISS ") + k)
print(f"score {sum(checks.values())}/{len(checks)}")
