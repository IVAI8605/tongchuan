#!/bin/bash
# 通传 NAS 守护进程在 fnOS 上的生命周期：start / stop / status / restart
# 由应用中心调用，也可手动 ./cmd/main restart。全部可写落点收进应用数据目录。
set -u
APP_NAME="${TRIM_APPNAME:-tongchuan}"
APP_DIR="${TRIM_APPDEST:-/var/apps/${APP_NAME}}"
DATA_DIR="${TRIM_PKGVAR:-/vol1/@appdata/${APP_NAME}}"
BIN="${APP_DIR}/bin/tc"
CFG="${DATA_DIR}/config.toml"
PID_FILE="${DATA_DIR}/daemon.pid"
OUT="${DATA_DIR}/logs/daemon.out"

mkdir -p "${DATA_DIR}" "${DATA_DIR}/identity" "${DATA_DIR}/logs" "${DATA_DIR}/queue" "${DATA_DIR}/received" 2>/dev/null

# 首次运行落一份可编辑配置：接收落点、端口都在这里，改完 restart 生效
if [ ! -f "${CFG}" ]; then
    cat > "${CFG}" <<CFG
# 通传守护进程配置（应用中心生成，可直接编辑，改完 ./cmd/main restart 生效）
alias = "通传NAS"
listen = "0.0.0.0:53318"
manage = "0.0.0.0:53319"
# 接收落点：默认落在应用数据目录。想在「文件管理」里方便看到，可改成你的共享目录，如 /vol1/1000/xxx
destination = "${DATA_DIR}/received"
allow = ["${DATA_DIR}/received"]
identity_dir = "${DATA_DIR}/identity"
log_dir = "${DATA_DIR}/logs"
queue_dir = "${DATA_DIR}/queue"
log_days = 2
log_level = "info"
trust_mode = "closed"
streams = 4
hash_cache = true
CFG
    chmod 600 "${CFG}"
fi

running() { [ -f "${PID_FILE}" ] && kill -0 "$(cat "${PID_FILE}")" 2>/dev/null; }

report() {
    local tok=""
    [ -r "${DATA_DIR}/identity/manage.token" ] && tok="$(cat "${DATA_DIR}/identity/manage.token")"
    local ip
    ip="$(ip -4 route get 1.1.1.1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p' | head -n1)"
    [ -n "${ip}" ] || ip="NAS的地址"
    echo "  传输端口:   ${ip}:53318（TCP+UDP）"
    echo "  管理页:     http://${ip}:53319/?t=${tok:-<见 identity/manage.token>}"
    echo "  接收落点:   ${DATA_DIR}/received（可在 ${CFG} 里改）"
    echo "  数据目录:   ${DATA_DIR}"
}

start() {
    if running; then echo "already running (pid $(cat "${PID_FILE}"))"; report; return 0; fi
    if [ ! -x "${BIN}" ]; then echo "ERROR: 程序不存在或不可执行: ${BIN}"; return 1; fi
    mkdir -p "${DATA_DIR}/logs"
    echo "===== $(date '+%Y-%m-%d %H:%M:%S') start =====" >> "${OUT}"
    nohup "${BIN}" daemon --config "${CFG}" >> "${OUT}" 2>&1 < /dev/null &
    echo $! > "${PID_FILE}"
    local i
    for i in $(seq 1 15); do
        if running && [ -r "${DATA_DIR}/identity/manage.token" ]; then break; fi
        sleep 1
    done
    if running; then echo "started (pid $(cat "${PID_FILE}"))"; report; return 0; fi
    echo "start failed, 详见 ${OUT}"; return 1
}

stop() {
    if running; then
        local pid; pid="$(cat "${PID_FILE}")"
        kill "${pid}" 2>/dev/null
        local i; for i in $(seq 1 10); do kill -0 "${pid}" 2>/dev/null || break; sleep 1; done
        kill -9 "${pid}" 2>/dev/null
    fi
    pkill -f "^${BIN} daemon" 2>/dev/null
    rm -f "${PID_FILE}"
    echo "stopped"
}

status() { if running; then echo "running (pid $(cat "${PID_FILE}"))"; return 0; else echo "stopped"; return 1; fi; }

case "${1:-start}" in
    start) start ;;
    stop) stop ;;
    status) status ;;
    restart) stop; sleep 1; start ;;
    *) echo "Usage: $0 {start|stop|status|restart}"; exit 1 ;;
esac
