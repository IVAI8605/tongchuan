# -*- coding: utf-8 -*-
"""解包 .fpk 并断言打包自洽：manifest 版本、checksum、内嵌 tc 版本、图标、脚本可执行位。

零依赖标准库。用法：
    python3 fpk/verify-fpk.py <包路径> <期望版本>

只读包，不落仓库；校验 tc 版本时把二进制写到 TMPDIR 下的临时文件，用完清理。
"""
import gzip
import hashlib
import io
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile


def fail(msg):
    print("FAIL " + msg)
    sys.exit(1)


def main():
    if len(sys.argv) != 3:
        fail("用法: verify-fpk.py <包路径> <期望版本>")
    path, expected = sys.argv[1], sys.argv[2]

    with open(path, "rb") as f:
        outer = tarfile.open(fileobj=io.BytesIO(gzip.decompress(f.read())), mode="r:")
    names = outer.getnames()
    for n in ("manifest", "app.tgz", "ui/config", "config/privilege", "config/resource"):
        if n not in names:
            fail("包内缺少 " + n)

    manifest = outer.extractfile("manifest").read().decode("utf-8")
    fields = {}
    for line in manifest.splitlines():
        if "=" in line and not line.lstrip().startswith("#"):
            k, v = line.split("=", 1)
            fields[k.strip()] = v.strip()
    if fields.get("version") != expected:
        fail("manifest 版本 %r != 期望 %s" % (fields.get("version"), expected))

    app_bytes = outer.extractfile("app.tgz").read()
    md5 = hashlib.md5(app_bytes).hexdigest()
    if fields.get("checksum") != md5:
        fail("manifest checksum %r != app.tgz md5 %s" % (fields.get("checksum"), md5))

    ui_cfg = outer.extractfile("ui/config").read().decode("utf-8")
    if '"icon": "images/icon_256.png"' not in ui_cfg:
        fail("桌面入口图标不是 images/icon_256.png")
    if '"icon_{0}.png"' in ui_cfg:
        fail("桌面入口图标仍是字面占位符 icon_{0}.png")

    for m in outer.getmembers():
        if m.isfile() and m.name.startswith("cmd/"):
            if m.mode & 0o111 == 0:
                fail("生命周期脚本缺可执行位: " + m.name)

    inner = tarfile.open(fileobj=io.BytesIO(gzip.decompress(app_bytes)), mode="r:")
    inner_names = inner.getnames()
    if "bin/tc" not in inner_names or "ui/images/icon_256.png" not in inner_names:
        fail("app.tgz 缺 bin/tc 或 ui/images/icon_256.png")

    fd, probe = tempfile.mkstemp(prefix="fpk-tc-probe-")
    os.close(fd)
    try:
        with open(probe, "wb") as out:
            out.write(inner.extractfile("bin/tc").read())
        os.chmod(probe, 0o755)
        r = subprocess.run([probe, "version"], capture_output=True, text=True, timeout=15)
        text = r.stdout.strip()
        embedded = text.splitlines()[0].strip() if text else ""
    finally:
        try:
            os.unlink(probe)
        except OSError:
            pass
    if r.returncode != 0 or embedded != expected:
        fail("内嵌 tc 版本 %r (exit=%d) != 期望 %s" % (embedded, r.returncode, expected))

    print("PASS version=%s checksum=%s icon=images/icon_256.png tc=%s" % (expected, md5, embedded))


if __name__ == "__main__":
    main()
