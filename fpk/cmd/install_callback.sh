#!/bin/bash
# 安装后：建数据目录、给二进制可执行位
set -u
APP_DIR="${TRIM_APPDEST:-/var/apps/${TRIM_APPNAME:-tongchuan}}"
DATA_DIR="${TRIM_PKGVAR:-/vol1/@appdata/${TRIM_APPNAME:-tongchuan}}"
mkdir -p "${DATA_DIR}" || exit 1
chmod 755 "${APP_DIR}/bin/tc" 2>/dev/null
echo "tongchuan installed: ${APP_DIR} ; data: ${DATA_DIR}"
exit 0
