"""Clean CSV rows on the cluster: python3 examples/sdk/csv_clean.py  (head from CK_HEAD, default 127.0.0.1:7702)."""
try:
    import _path  # noqa: F401  (runs from a checkout without pip install; the PCs have no _path, nor need it)
except ImportError:
    pass
import clusterkit as ck


def clean_row(line: str) -> dict[str, object]:
    name, age, city = [f.strip() for f in line.split(",")]
    s = 0
    for i in range(200_000):  # stand-in for real work
        s += i % 7
    return {"name": name.title(), "age": int(age) if age.isdigit() else None, "city": city.upper(), "k": s}


if __name__ == "__main__":
    rows = [f"  ann{i} , {20 + i % 50} ,paris" for i in range(400)]
    print(ck.plan(clean_row, rows))
    out = ck.map(clean_row, rows)
    assert out == [clean_row(r) for r in rows]
    print(len(out), "rows cleaned; first:", out[0])
