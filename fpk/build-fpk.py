# -*- coding: utf-8 -*-
"""把通传（tongchuan）打成 fnOS（飞牛 OS）可安装的 .fpk。

零依赖，只用 Python 标准库（Linux / Windows 都能跑）：
    python3 fpk/build-fpk.py

结构照抄本机已跑通的 Magpie-fpk / DiskSentry 打包方式：
    fpk = gzip(tar)
      ├── manifest          应用元信息，checksum 是 app.tgz 的 md5
      ├── app.tgz           应用本体（bin/tc 二进制 + ui 配置与图标）
      ├── cmd/*             生命周期脚本，必须带可执行位
      ├── config/privilege  以 root 运行（要监听端口、写接收落点）
      ├── config/resource   声明的数据共享
      ├── ICON.PNG / ICON_256.PNG   应用中心列表图标
      └── ui/               桌面入口配置与图标

版本号由 APP_VERSION 决定（用户 2026-11-23 定为 1.0.0）。
产物：<项目根>/dist/tongchuan-<ver>-linux-x64.fpk
"""
import gzip
import hashlib
import io
import math
import os
import struct
import tarfile
import zlib

APP_NAME = "tongchuan"
# 版本号唯一来源：根 Cargo.toml（三端统一）。仅改版号、无代码改动时可跳过测试。
def _cargo_version():
    import re as _re
    src = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))  # fpk -> src
    txt = open(os.path.join(src, "Cargo.toml"), encoding="utf-8").read()
    m = _re.search(r'(?m)^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"', txt)
    return m.group(1) if m else "0.0.0"

APP_VERSION = os.environ.get("TC_FPK_VERSION") or _cargo_version()
DISPLAY_NAME = "通传 · 局域网互传"
WEB_PORT = 53319          # 管理页端口（桌面入口指向它）
XFER_PORT = 53318         # 传输端口（TCP+UDP）

# src/fpk/build-fpk.py -> ROOT = src/（仓库根），项目根 = 上一级（tongchuan/）
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)                       # src/
PROJECT = os.path.dirname(ROOT)                    # tongchuan/
CMD_DIR = os.path.join(HERE, "cmd")
# tc 二进制在跨项目 _tool 构建目录（AI/_tool/build/...），即 src/../../_tool
VENDOR_BIN = os.path.join(ROOT, "..", "..", "_tool", "build", "tongchuan-target", "release", "tc")
# 允许用环境变量覆盖二进制路径（CI / 交叉构建时用）
VENDOR_BIN = os.environ.get("TC_BIN", VENDOR_BIN)
OUT = os.path.join(PROJECT, "dist")
os.makedirs(OUT, exist_ok=True)

UID, GID, UNAME = 1001, 1001, "runner"


def read_text(path):
    with open(path, "rb") as f:
        data = f.read()
    if data.startswith(b"\xef\xbb\xbf"):
        data = data[3:]
    return data.replace(b"\r\n", b"\n")


def png_chunk(tag, data):
    return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)


def _in_arrow(x, y, W, H, up):
    """上箭头指向右、下箭头指向左——表达「互传」。返回该像素是否落在箭头里。"""
    if up:
        by0, by1 = 0.30 * H, 0.42 * H
        bx0, bx1 = 0.22 * W, 0.66 * W
        tx0, tx1 = 0.60 * W, 0.80 * W
        cy, half = 0.36 * H, 0.14 * H
        if by0 <= y <= by1 and bx0 <= x <= bx1:
            return True
        if tx0 <= x <= tx1:
            frac = (x - tx0) / (tx1 - tx0)
            return abs(y - cy) <= half * (1 - frac)
        return False
    else:
        by0, by1 = 0.58 * H, 0.70 * H
        bx0, bx1 = 0.34 * W, 0.78 * W
        tx0, tx1 = 0.20 * W, 0.40 * W
        cy, half = 0.64 * H, 0.14 * H
        if by0 <= y <= by1 and bx0 <= x <= bx1:
            return True
        if tx0 <= x <= tx1:
            frac = (tx1 - x) / (tx1 - tx0)
            return abs(y - cy) <= half * (1 - frac)
        return False


def make_icon(size):
    """墨绿圆角底 + 一对白色双向箭头（互为收发）。"""
    W = H = size
    top = (31, 111, 92)      # #1F6F5C 通传主色
    bottom = (20, 74, 61)
    fg = (245, 245, 245)
    rad = size * 0.20
    rows = []
    for y in range(H):
        row = bytearray([0])
        for x in range(W):
            mx = min(x, W - 1 - x)
            my = min(y, H - 1 - y)
            if mx < rad and my < rad:
                dx = rad - mx
                dy = rad - my
                if dx * dx + dy * dy > rad * rad:
                    row += bytes((0, 0, 0, 0))
                    continue
            t = y / float(H - 1)
            px = tuple(int(top[i] + (bottom[i] - top[i]) * t) for i in range(3))
            if _in_arrow(x + 0.5, y + 0.5, W, H, True) or _in_arrow(x + 0.5, y + 0.5, W, H, False):
                px = fg
            row += bytes((px[0], px[1], px[2], 255))
        rows.append(bytes(row))
    ihdr = struct.pack(">IIBBBBB", W, H, 8, 6, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + png_chunk(b"IHDR", ihdr)
            + png_chunk(b"IDAT", zlib.compress(b"".join(rows), 9)) + png_chunk(b"IEND", b""))


def gz(data, level=6):
    buf = io.BytesIO()
    with gzip.GzipFile(fileobj=buf, mode="wb", compresslevel=level, mtime=0) as g:
        g.write(data)
    return buf.getvalue()


def tar_add(tf, name, data=None, mode=0o644, isdir=False):
    ti = tarfile.TarInfo(name)
    ti.uid = UID
    ti.gid = GID
    ti.uname = UNAME
    ti.gname = UNAME
    ti.mtime = 0
    if isdir:
        ti.type = tarfile.DIRTYPE
        ti.mode = 0o755
        ti.size = 0
        tf.addfile(ti)
    else:
        ti.mode = mode
        ti.size = len(data)
        tf.addfile(ti, io.BytesIO(data))


UI_CONFIG = (
    '{\n'
    '  ".url": {\n'
    '    "' + APP_NAME + '.Application": {\n'
    '      "title": "' + DISPLAY_NAME + '",\n'
    '      "desc": "\u5c40\u57df\u7f51\u6587\u4ef6\u5939\u4e92\u4f20\uff1a\u7535\u8111/\u624b\u673a/NAS \u4e4b\u95f4\u76f4\u63a5\u4f20\uff0c\u4e0d\u8d70\u4e91\u7aef",\n'
    '      "icon": "images/icon_{0}.png",\n'
    '      "type": "url",\n'
    '      "port": "' + str(WEB_PORT) + '",\n'
    '      "protocol": "http",\n'
    '      "url": "/",\n'
    '      "allUsers": true\n'
    '    }\n'
    '  }\n'
    '}\n'
)
PRIVILEGE = '{\n  "defaults": {\n    "run-as": "root"\n  }\n}\n'
RESOURCE = (
    '{\n'
    '  "data-share": {\n'
    '    "shares": [\n'
    '      {\n'
    '        "name": "' + APP_NAME + '",\n'
    '        "permission": { "rw": ["' + APP_NAME + '"] }\n'
    '      }\n'
    '    ]\n'
    '  }\n'
    '}\n'
)

MANIFEST = (
    "appname         = " + APP_NAME + "\n"
    "version         = {ver}\n"
    "display_name    = " + DISPLAY_NAME + "\n"
    "desc            = <p><b>通传</b>（局域网互传）：电脑、手机与 NAS 之间直接传文件夹，不走云端。</p><p>接收文件默认落在 /vol2/1000/好3T/通传接收；安全功能默认关闭（同网段发现即可传），要严格指纹配对在数据目录 config.toml 里改 trust_mode=closed。</p>\n"
    "arch            = x86_64\n"
    "source          = thirdparty\n"
    "desktop_uidir   = ui\n"
    "desktop_applaunchname = " + APP_NAME + ".Application\n"
    "maintainer      = IVAI8605\n"
    "maintainer_url  = https://github.com/IVAI8605/tongchuan\n"
    "distributor     = IVAI8605\n"
    "distributor_url = https://github.com/IVAI8605/tongchuan\n"
    "service_port    = " + str(WEB_PORT) + "\n"
    "ctl_stop        = true\n"
    "platform        = \n"
    "install_type    = root\n"
    "checksum        = {md5}\n"
)


def build():
    if not os.path.exists(VENDOR_BIN):
        raise SystemExit("缺少二进制：%s（先 cargo build --release -p tongchuan）" % VENDOR_BIN)
    with open(VENDOR_BIN, "rb") as f:
        binary = f.read()
    if binary[:4] != b"\x7fELF":
        raise SystemExit("二进制不是 ELF：%s" % VENDOR_BIN)

    # 内层 app.tgz
    app_tar = io.BytesIO()
    with tarfile.open(fileobj=app_tar, mode="w") as atf:
        tar_add(atf, "bin", isdir=True)
        tar_add(atf, "bin/tc", binary, 0o755)
        tar_add(atf, "ui", isdir=True)
        tar_add(atf, "ui/config", UI_CONFIG.encode("utf-8"))
        tar_add(atf, "ui/images", isdir=True)
        tar_add(atf, "ui/images/icon_64.png", make_icon(64))
        tar_add(atf, "ui/images/icon_128.png", make_icon(128))
        tar_add(atf, "ui/images/icon_256.png", make_icon(256))
    app_tgz = gz(app_tar.getvalue())
    md5 = hashlib.md5(app_tgz).hexdigest()

    scripts = {
        "install_init": read_text(os.path.join(CMD_DIR, "install_init.sh")),
        "install_callback": read_text(os.path.join(CMD_DIR, "install_callback.sh")),
        "main": read_text(os.path.join(CMD_DIR, "main.sh")),
        "uninstall_init": read_text(os.path.join(CMD_DIR, "uninstall_init.sh")),
        "uninstall_callback": read_text(os.path.join(CMD_DIR, "uninstall_callback.sh")),
        "upgrade_init": read_text(os.path.join(CMD_DIR, "upgrade_init.sh")),
        "upgrade_callback": read_text(os.path.join(CMD_DIR, "upgrade_callback.sh")),
    }

    out_tar = io.BytesIO()
    with tarfile.open(fileobj=out_tar, mode="w") as tf:
        tar_add(tf, "app.tgz", app_tgz)
        tar_add(tf, "cmd", isdir=True)
        for name, data in scripts.items():
            tar_add(tf, "cmd/" + name, data, 0o755)
        tar_add(tf, "config", isdir=True)
        tar_add(tf, "config/privilege", PRIVILEGE.encode("utf-8"))
        tar_add(tf, "config/resource", RESOURCE.encode("utf-8"))
        tar_add(tf, "ICON.PNG", make_icon(128))
        tar_add(tf, "ICON_256.PNG", make_icon(256))
        tar_add(tf, "manifest", MANIFEST.format(ver=APP_VERSION, md5=md5).encode("utf-8"))
        tar_add(tf, "ui", isdir=True)
        tar_add(tf, "ui/config", UI_CONFIG.encode("utf-8"))
        tar_add(tf, "ui/images", isdir=True)
        tar_add(tf, "ui/images/64.png", make_icon(64))
        tar_add(tf, "ui/images/256.png", make_icon(256))

    out_path = os.path.join(OUT, "%s-%s-linux-x64.fpk" % (APP_NAME, APP_VERSION))
    with open(out_path, "wb") as f:
        f.write(gz(out_tar.getvalue(), 9))
    digest = hashlib.sha256(open(out_path, "rb").read()).hexdigest()
    with open(out_path + ".sha256", "w") as f:
        f.write(digest + "  " + os.path.basename(out_path) + "\n")
    print("built:", out_path)
    print("size: %.1f MB" % (os.path.getsize(out_path) / 1048576.0))
    print("sha256:", digest)
    print("app.tgz md5:", md5)
    return out_path


if __name__ == "__main__":
    build()
