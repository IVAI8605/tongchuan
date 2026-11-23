#!/bin/bash
# 升级前：停旧进程，避免覆盖运行中的二进制
set -u
APP_DIR="${TRIM_APPDEST:-/var/apps/${TRIM_APPNAME:-tongchuan}}"
[ -x "${APP_DIR}/cmd/main" ] && "${APP_DIR}/cmd/main" stop
exit 0
