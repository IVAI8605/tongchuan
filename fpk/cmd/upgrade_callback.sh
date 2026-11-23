#!/bin/bash
# 升级后：修可执行位与数据目录，配置与数据保持原状
set -u
APP_DIR="${TRIM_APPDEST:-/var/apps/${TRIM_APPNAME:-tongchuan}}"
DATA_DIR="${TRIM_PKGVAR:-/vol1/@appdata/${TRIM_APPNAME:-tongchuan}}"
chmod 755 "${APP_DIR}/bin/tc" 2>/dev/null
mkdir -p "${DATA_DIR}" 2>/dev/null
echo "tongchuan upgraded; data: ${DATA_DIR}"
exit 0
