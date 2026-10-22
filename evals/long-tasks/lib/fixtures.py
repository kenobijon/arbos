"""Generated fixtures: deterministic data files plus the truth the grader compares against.
Each generator returns (files: {relative path: bytes}, truth: dict)."""

import csv
import hashlib
import io
import json
import random
import zlib


def sales_csv(seed=20261022):
    rng = random.Random(seed)
    products = ["Acacia Desk", "Birch Chair", "Cedar Shelf", "Dogwood Lamp", "Elm Table", "Fir Stool"]
    rows = []
    monthly = {}
    per_product = {}
    total = 0.0
    for i in range(500):
        month = f"2026-{rng.randint(1, 9):02d}"
        day = rng.randint(1, 28)
        p = rng.choice(products)
        qty = rng.randint(1, 6)
        price = rng.choice([19.5, 24.0, 49.99, 89.0, 120.0, 249.0])
        rev = round(qty * price, 2)
        rows.append([f"{month}-{day:02d}", f"ORD-{1000 + i}", p, qty, f"{price:.2f}", f"{rev:.2f}"])
        total += rev
        monthly[month] = round(monthly.get(month, 0.0) + rev, 2)
        per_product[p] = round(per_product.get(p, 0.0) + rev, 2)
    buf = io.StringIO()
    w = csv.writer(buf)
    w.writerow(["date", "order_id", "product", "quantity", "unit_price", "revenue"])
    w.writerows(rows)
    top3 = [p for p, _ in sorted(per_product.items(), key=lambda kv: -kv[1])[:3]]
    return {"sales.csv": buf.getvalue().encode()}, {"total": round(total, 2), "top3": top3, "monthly": monthly, "per_product": per_product}


def service_log(seed=7):
    rng = random.Random(seed)
    services = ["auth", "billing", "relay", "search", "mailer"]
    levels = ["info", "info", "info", "warn", "error"]
    counts = {s: 0 for s in services}
    lines = []
    for i in range(3000):
        s = rng.choice(services)
        lvl = rng.choice(levels)
        if lvl == "error":
            counts[s] += 1
        lines.append(json.dumps({"ts": 1792600000 + i * 7, "service": s, "level": lvl, "msg": f"event {i}"}))
    return {"app.log": ("\n".join(lines) + "\n").encode()}, {"errors": counts}


def dedupe_file(seed=3):
    rng = random.Random(seed)
    words = [f"token-{rng.randint(1, 9000):04d}" for _ in range(50000)]
    uniq = sorted(set(words))
    sha = hashlib.sha256(("\n".join(uniq) + "\n").encode()).hexdigest()
    return {"words.txt": ("\n".join(words) + "\n").encode()}, {"unique_lines": len(uniq), "sha256": sha}


def _pdf(lines):
    """A small valid PDF with one page of Helvetica text (uncompressed, so any extractor works)."""
    content = "BT /F1 12 Tf 50 750 Td 16 TL " + " ".join(f"({l.replace('(', '').replace(')', '')}) Tj T*" for l in lines) + " ET"
    objs = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
        f"<< /Length {len(content)} >>\nstream\n{content}\nendstream",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    ]
    out = "%PDF-1.4\n"
    offsets = []
    for i, o in enumerate(objs, 1):
        offsets.append(len(out))
        out += f"{i} 0 obj\n{o}\nendobj\n"
    xref = len(out)
    out += f"xref\n0 {len(objs) + 1}\n0000000000 65535 f \n" + "".join(f"{o:010d} 00000 n \n" for o in offsets)
    out += f"trailer\n<< /Size {len(objs) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
    return out.encode("latin-1")


def invoice_pdf():
    lines = [
        "ARBOS SUPPLY CO.",
        "Invoice number: INV-2026-0931",
        "Date: 2026-10-14",
        "Bill to: Priya Natarajan, 14 Cedar Lane",
        "Item: Birch Chair x 4 at 89.00 = 356.00",
        "Item: Dogwood Lamp x 2 at 49.99 = 99.98",
        "Subtotal: 455.98",
        "Tax (13%): 59.28",
        "TOTAL DUE: 515.26 USD",
        "Payment terms: net 30",
    ]
    return {"invoice.pdf": _pdf(lines)}, {"invoice_number": "INV-2026-0931", "total": "515.26"}


def chunks(n=20):
    files = {}
    for i in range(1, n + 1):
        files[f"chunks/chunk-{i:02d}.txt"] = f"chunk {i}\n{'x' * (100 * i)}\n".encode()
    return files, {"n": n}


GENERATORS = {
    "sales_csv": sales_csv,
    "service_log": service_log,
    "dedupe_file": dedupe_file,
    "invoice_pdf": invoice_pdf,
    "chunks": chunks,
}


def crc(b):
    return zlib.crc32(b) & 0xFFFFFFFF
