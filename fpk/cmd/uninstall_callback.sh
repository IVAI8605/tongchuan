#!/bin/bash
# 卸载后：只提示数据位置，不删任何数据（身份/信任/已收文件都留着，重装可用）
set -u
DATA_DIR="${TRIM_PKGVAR:-/vol1/@appdata/${TRIM_APPNAME:-tongchuan}}"
echo "tongchuan uninstalled; data kept at ${DATA_DIR}"
exit 0
