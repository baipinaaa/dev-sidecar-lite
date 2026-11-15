import io, glob, re, sys


def strip(code):
    out = []
    i = 0
    n = len(code)
    while i < n:
        c = code[i]
        if c == "/" and i + 1 < n and code[i + 1] == "/":
            while i < n and code[i] != "\n":
                i += 1
        elif c == "/" and i + 1 < n and code[i + 1] == "*":
            i += 2
            while i + 1 < n and not (code[i] == "*" and code[i + 1] == "/"):
                i += 1
            i += 2
        elif c == "r" and i + 1 < n and (code[i + 1] == '"' or code[i + 1] == "#"):
            j = i + 1
            hashes = 0
            while j < n and code[j] == "#":
                hashes += 1
                j += 1
            if j < n and code[j] == '"':
                close = '"' + "#" * hashes
                k = code.find(close, j + 1)
                i = (k + len(close)) if k >= 0 else n
            else:
                out.append(c)
                i += 1
        elif c == '"':
            i += 1
            while i < n:
                if code[i] == "\\":
                    i += 2
                    continue
                if code[i] == '"':
                    i += 1
                    break
                i += 1
        elif c == "'":
            m = re.match(r"'(\\.|[^\\'])'", code[i:])
            if m:
                i += len(m.group(0))
            else:
                i += 1
        else:
            out.append(c)
            i += 1
    return "".join(out)


files = sorted(
    glob.glob("crates/**/*.rs", recursive=True)
    + glob.glob("app/src-tauri/**/*.rs", recursive=True)
)
bad = 0
for f in files:
    src = io.open(f, encoding="utf-8").read()
    s = strip(src)
    for a, b in [("{", "}"), ("(", ")"), ("[", "]")]:
        if s.count(a) != s.count(b):
            print(f"{f}: {a}{b} {s.count(a)} vs {s.count(b)}")
            bad += 1
    if "\t" in src:
        print(f"{f}: 含 tab")
print(f"files={len(files)} imbalanced={bad}")
