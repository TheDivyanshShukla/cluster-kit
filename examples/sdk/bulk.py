"""Bulk distribution: shard a big file into blobs and count lines per shard on the PCs: python3 examples/sdk/bulk.py [path] [parts]"""
import _path  # noqa: F401
import os, sys, tempfile
import clusterkit as ck


def count_lines(path: str) -> int:  # fn gets the local path of one shard on the PC
    with open(path, "rb") as f:
        return sum(1 for _ in f)


if __name__ == "__main__":
    path = sys.argv[1] if len(sys.argv) > 1 else os.path.join(tempfile.gettempdir(), "ck-bulk-demo.txt")
    if not os.path.exists(path):
        with open(path, "w") as f:
            f.writelines(f"line {i} " + "x" * 200 + "\n" for i in range(200_000))
    shards = ck.shard(path, parts=int(sys.argv[2]) if len(sys.argv) > 2 else 20)  # uploaded once; a second run moves nothing
    print(ck.plan(count_lines, shards).transfer())
    print(sum(ck.map(count_lines, shards)), "lines")
