#!/bin/bash
# 通传 NAS 守护进程在 fnOS 上的生命周期：start / stop / status / restart
# 由应用中心调用，也可手动 ./cmd/main restart。可写数据全部收进应用数据目录，不写死任何私有卷路径。
set -u
APP_NAME="${TRIM_APPNAME:-tongchuan}"
APP_DIR="${TRIM_APPDEST:-/var/apps/${APP_NAME}}"
DATA_DIR="${TRIM_PKGVAR:-/vol1/@appdata/${APP_NAME}}"
BIN="${APP_DIR}/bin/tc"
CFG="${DATA_DIR}/config.toml"
PID_FILE="${DATA_DIR}/daemon.pid"
OUT="${DATA_DIR}/logs/daemon.out"
# 默认接收落点：应用数据目录下的 received/。保证可写、不指向任何特定机器的私有卷；
# 用户可在管理页或 config.toml 把它改到某个共享目录。
RECV_DEFAULT="${DATA_DIR}/received"

mkdir -p "${DATA_DIR}" "${DATA_DIR}/identity" "${DATA_DIR}/logs" "${DATA_DIR}/queue" "${RECV_DEFAULT}" 2>/dev/null

# 可选：管理员若把旧设备身份放到 ${DATA_DIR}/.identity-import，首次启动前导入以延续原指纹（升级迁移用）。
# 私钥不进 fpk 包，只走本机数据目录；导入后把 import 目录改名留档。
IMPORT_DIR="${DATA_DIR}/.identity-import"
if [ -d "${IMPORT_DIR}" ] && [ -f "${IMPORT_DIR}/identity.cer" ]; then
    cp -f "${IMPORT_DIR}/identity.cer" "${DATA_DIR}/identity/identity.cer" 2>/dev/null
    cp -f "${IMPORT_DIR}/identity.key" "${DATA_DIR}/identity/identity.key" 2>/dev/null
    [ -f "${IMPORT_DIR}/trust.jsonl" ] && cp -f "${IMPORT_DIR}/trust.jsonl" "${DATA_DIR}/identity/trust.jsonl" 2>/dev/null
    chmod 600 "${DATA_DIR}/identity"/* 2>/dev/null
    mv "${IMPORT_DIR}" "${IMPORT_DIR}.done.$(date +%s)" 2>/dev/null || true
    echo "imported identity from ${IMPORT_DIR}"
fi

# 首次运行落一份可编辑配置：接收落点、端口都在这里，改完 restart 生效。
# 落点：用 data-share 共享目录（系统创建并对应用用户 ACL 授权，用户在文件管理器可见），可在其下加一个子目录名。
SHARE_DIR="${TRIM_DATA_SHARE_PATHS%%:*}"
[ -n "${SHARE_DIR}" ] || SHARE_DIR="${DATA_DIR}"
SUB="通传接收"
if [ -s "${DATA_DIR}/.install-subdir" ]; then
    _s="$(head -n1 "${DATA_DIR}/.install-subdir" 2>/dev/null | tr -d '\r' | sed 's#[/\\].*##; s/^[[:space:]]*//; s/[[:space:]]*$//')"
    [ -n "${_s}" ] && [ "${_s}" != "." ] && [ "${_s}" != ".." ] && SUB="${_s}"
fi
DEST="${SHARE_DIR}/${SUB}"
[ -d "${DEST}" ] || mkdir -p "${DEST}" 2>/dev/null
if [ ! -f "${CFG}" ]; then
    cat > "${CFG}" <<CFG
# 通传守护进程配置（应用中心生成，可直接编辑，改完 ./cmd/main restart 生效）
alias = "通传NAS"
listen = "0.0.0.0:53318"
manage = "0.0.0.0:53319"
# 接收落点：数据共享目录下的子目录（首次安装可命名）。要改就改这里并让 allow 同步。
destination = "${DEST}"
allow = ["${DEST}"]
identity_dir = "${DATA_DIR}/identity"
log_dir = "${DATA_DIR}/logs"
queue_dir = "${DATA_DIR}/queue"
log_days = 2
log_level = "info"
# 安全功能默认关闭 = 信任校验关闭：同网段发现即可传（自动信任）。要严格指纹配对，改成 "closed"。
trust_mode = "open"
streams = 4
hash_cache = true
CFG
    chmod 600 "${CFG}"
fi

# 升级迁移：已有配置但旧落点不可写（如从 root 部署/私有卷迁来），自动改到共享目录，避免无法接收。
if [ -f "${CFG}" ]; then
    _cur="$(sed -n 's/^destination[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p' "${CFG}" 2>/dev/null | head -n1)"
    if [ -n "${_cur}" ] && ! { mkdir -p "${_cur}" 2>/dev/null && [ -w "${_cur}" ]; }; then
        sed -i "s#^destination[[:space:]]*=.*#destination = \"${DEST}\"#; s#^allow[[:space:]]*=.*#allow = [\"${DEST}\"]\]#" "${CFG}" 2>/dev/null \
            && echo "旧落点不可写，已改到 ${DEST}"
    fi
fi

running() { [ -f "${PID_FILE}" ] && kill -0 "$(cat "${PID_FILE}")" 2>/dev/null; }

cur_dest() {
    local d
    d="$(sed -n 's/^destination[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p' "${CFG}" 2>/dev/null | head -n1)"
    echo "${d:-${RECV_DEFAULT}}"
}

report() {
    local ip
    ip="$(ip -4 route get 1.1.1.1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p' | head -n1)"
    [ -n "${ip}" ] || ip="NAS的地址"
    echo "  传输端口:   ${ip}:53318（TCP+UDP）"
    echo "  管理页:     http://${ip}:53319/（首次打开设置管理密码，之后密码登录）"
    echo "  接收落点:   $(cur_dest)（可在 ${CFG} 或管理页改）"
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
    for i in $(seq 1 20); do
        running || { sleep 1; continue; }
        # 进程在且端口就绪即算起来（有 curl 就探一下 /api/health，没有就直接放过）
        if command -v curl >/dev/null 2>&1; then
            curl -fsS -o /dev/null "http://127.0.0.1:53319/api/health" 2>/dev/null && break
        else
            break
        fi
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

status() { if running; then echo "running (pid $(cat "${PID_FILE}"))"; return 0; else echo "stopped"; return 3; fi; }

case "${1:-start}" in
    start) start ;;
    stop) stop ;;
    status) status ;;
    restart) stop; sleep 1; start ;;
    *) echo "Usage: $0 {start|stop|status|restart}"; exit 1 ;;
esac
