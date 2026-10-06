#!/usr/bin/env python3
"""Deterministic fixture for the business-operations hard trial.

A fictional ceramics shop with one week of messy books. Seven problems are
planted; grade-kettle-kiln.py checks which of them the agent surfaced.
"""
import csv, json, random, pathlib, datetime as dt
R = random.Random(7)
root = pathlib.Path(__file__).parent / "kettle-kiln-ops" / "business"
(root / "inbox").mkdir(parents=True, exist_ok=True)
skus = {  # sku: (name, price, unit_cost_old, weekly_units, on_hand, lead_days)
    "KK-MUG-12": ("Speckled stoneware mug 12oz", 28.0, 7.10, 60, 180, 21),
    "KK-MUG-16": ("Tall latte mug 16oz", 32.0, 8.40, 35, 120, 21),
    "KK-BWL-SM": ("Ramen bowl small", 36.0, 9.80, 22, 70, 28),
    "KK-PLT-10": ("Dinner plate 10in", 30.0, 8.90, 18, 90, 28),
    "KK-VAS-BUD": ("Bud vase", 24.0, 5.60, 41, 70, 14),       # planted: runs out inside its lead time
    "KK-TPT-1L": ("Teapot 1L", 78.0, 21.00, 9, 40, 35),
    "KK-GFT-SET": ("Gift set: 2 mugs + pour-over", 88.0, 24.50, 14, 45, 21),
}
new_costs = dict((k, v[2]) for k, v in skus.items()); new_costs["KK-GFT-SET"] = 24.50
new_costs["KK-PLT-10"] = 8.90; new_costs["KK-TPT-1L"] = 21.00
new_costs["KK-BWL-SM"] = 9.80
new_costs["KK-MUG-16"] = 8.40
# planted: supplier raised the gift set cost so it loses money after free shipping
new_costs["KK-GFT-SET"] = 61.40
start = dt.datetime(2026, 9, 14, 8, 0)
customers = [f"{f} {l}" for f in ["Ana","Ben","Chloe","Dev","Emi","Farah","Gus","Hana","Ivan","Jo","Kai","Lena","Milo","Nia","Omar","Pia","Quinn","Rosa","Sam","Tara"] for l in ["Reyes","Okafor","Lind","Patel","Moreau","Kim"]]
orders, oid = [], 4100
for day in range(7):
    for _ in range(R.randint(24, 34)):
        oid += 1
        sku = R.choices(list(skus), weights=[s[3] for s in skus.values()])[0]
        qty = R.choice([1, 1, 1, 2])
        t = start + dt.timedelta(days=day, minutes=R.randint(0, 14 * 60))
        subtotal = skus[sku][1] * qty
        ship = 0.0 if subtotal >= 75 else 7.95
        orders.append(dict(order_id=f"KK{oid}", created_at=t.isoformat(), customer=R.choice(customers),
                           email="", sku=sku, qty=qty, subtotal=f"{subtotal:.2f}", shipping_charged=f"{ship:.2f}",
                           total=f"{subtotal+ship:.2f}", status="fulfilled", payout_batch=f"PB-{(start+dt.timedelta(days=day)).strftime('%m%d')}"))
for o in orders: o["email"] = o["customer"].lower().replace(" ", ".") + "@example.com"
# planted: duplicate charge — same customer/items two minutes apart, both paid
dup_src = orders[57]; dup = dict(dup_src); dup["order_id"] = "KK" + str(oid + 1)
dup["created_at"] = (dt.datetime.fromisoformat(dup_src["created_at"]) + dt.timedelta(minutes=2)).isoformat()
orders.insert(58, dup)
# planted: an order returned to sender but marked fulfilled; customer asks for refund
rts = orders[140]; rts["status"] = "fulfilled"
orders.sort(key=lambda o: o["created_at"])
with open(root / "orders.csv", "w", newline="") as f:
    w = csv.DictWriter(f, fieldnames=list(orders[0])); w.writeheader(); w.writerows(orders)
# processor payouts: fee 2.9% + 0.30; planted: PB-0917 short by three orders
payouts = {}
for o in orders:
    payouts.setdefault(o["payout_batch"], []).append(o)
missing = [o["order_id"] for o in payouts["PB-0917"][5:8]]
bank = []
for batch, rows in sorted(payouts.items()):
    net = sum(float(o["total"]) * 0.971 - 0.30 for o in rows if o["order_id"] not in (missing if batch == "PB-0917" else []))
    d = dt.date(2026, int(batch[3:5]), int(batch[5:7])) + dt.timedelta(days=2)
    bank.append(dict(date=d.isoformat(), description=f"STRIPE PAYOUT {batch}", amount=f"{net:.2f}"))
for d, desc, amt in [("2026-09-15", "SHIPPO INC MONTHLY", "-49.00"), ("2026-09-15", "SHIPSTATION SUBSCRIPTION", "-59.99"),
                     ("2026-09-16", "USPS POSTAGE BATCH", "-612.40"), ("2026-09-18", "CLAYWORKS SUPPLY INV 2291", "-3120.00"),
                     ("2026-09-19", "USPS POSTAGE BATCH", "-588.15"), ("2026-09-19", "META ADS", "-420.00"),
                     ("2026-09-20", "SQUARESPACE", "-33.00"), ("2026-09-21", "OWNER DRAW", "-1500.00")]:
    bank.append(dict(date=d, description=desc, amount=amt))
bank.sort(key=lambda r: r["date"])
with open(root / "bank-statement.csv", "w", newline="") as f:
    w = csv.DictWriter(f, fieldnames=["date", "description", "amount"]); w.writeheader(); w.writerows(bank)
with open(root / "inventory.csv", "w", newline="") as f:
    w = csv.writer(f); w.writerow(["sku", "name", "price", "on_hand_start_of_week", "supplier_lead_days", "reorder_moq"])
    for k, v in skus.items(): w.writerow([k, v[0], f"{v[1]:.2f}", v[4], v[5], 48])
with open(root / "supplier-price-list-2026-09.csv", "w", newline="") as f:
    w = csv.writer(f); w.writerow(["sku", "unit_cost", "effective"])
    for k in skus: w.writerow([k, f"{new_costs[k]:.2f}", "2026-09-15"])
(root / "notes-from-owner.md").write_text("""# Notes
- Shipping is free over $75, otherwise $7.95. Real postage averages $9.40 per order.
- Payouts land two days after the batch date.
- We use one shipping tool. I think it's Shippo.
- Margin floor: nothing should sell below 35% contribution margin after postage and card fees.
""")
mails = {
 "01-refund-not-arrived.md": f"From: {rts['email']}\nSubject: where is my order {rts['order_id']}??\n\nIt's been 9 days. Tracking says 'returned to sender - insufficient address'. I want a refund or a reship, I moved last month. New address: 22 Alder Ct, Portland OR 97214.\n",
 "02-double-charged.md": f"From: {dup_src['email']}\nSubject: charged twice\n\nMy card shows two charges for the same order this week. Can you fix it?\n",
 "03-wholesale.md": "From: buyer@fernandfoundry.example\nSubject: Wholesale inquiry — bud vases\n\nWe run 4 cafés and want 200 bud vases for a November opening, net-30 terms. What's your wholesale price and can you deliver by Oct 20?\n",
 "04-angry-chargeback.md": "From: m.harlow@example.com\nSubject: FINAL WARNING\n\nI never ordered anything from you. I'm disputing the charge with my bank today.\n",
 "05-supplier.md": "From: accounts@clayworks.example\nSubject: Price update effective Sept 15\n\nHi! Heads up that our updated price list (attached as supplier-price-list-2026-09.csv) took effect Sept 15. Gift-set components now include the ceramic dripper, which is where most of the change is.\n",
 "06-press.md": "From: editor@hearthmag.example\nSubject: Holiday gift guide\n\nWe'd love to feature your gift set in our holiday guide (circulation 180k). Can you confirm price and that you can handle ~300 extra orders in December?\n",
}
for name, body in mails.items(): (root / "inbox" / name).write_text(body)
answers = {
 "duplicate_order": [dup_src["order_id"], dup["order_id"]],
 "returned_to_sender": rts["order_id"],
 "payout_short_batch": "PB-0917", "payout_missing_orders": missing,
 "negative_margin_sku": "KK-GFT-SET",
 "stockout_sku": "KK-VAS-BUD",
 "duplicate_saas": ["SHIPSTATION", "SHIPPO"],
 "chargeback_email": "m.harlow@example.com",
}
(root.parent / "answers.json").write_text(json.dumps(answers, indent=1))
print(json.dumps(answers))
