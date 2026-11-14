package com.tongchuan;

/** Rust 内核的 JNI 入口。方法名必须和 crates/tc-jni 里的导出符号一一对应，改动要同步两边。 */
public final class TcNative {

    static {
        System.loadLibrary("tc_jni");
    }

    private TcNative() {
    }

    /** 启动常驻接收端，返回句柄；失败返回负数。 */
    public static native long startReceiver(String configJson);

    public static native boolean stopReceiver(long handle);

    /** 返回 JSON 字符串（running / sessions / bytes / listen / lastError），句柄无效时返回 null。 */
    public static native String statusJson(long handle);

    /**
     * 信任库快照：{"peers":[...],"pending":[...]}。传 identityDir，
     * 与接收端门禁读的是同一个 trust.jsonl；失败返回 null。
     */
    public static native String trustJson(String identityDir);

    /** 批准指定指纹；返回 false 表示写入失败（指纹非法或磁盘错误）。 */
    public static native boolean trustApprove(String identityDir, String fingerprint);

    /** 撤销信任；返回 false 表示该指纹本来就不在信任表里。 */
    public static native boolean trustRevoke(String identityDir, String fingerprint);

    /**
     * 列目录（M1 路径模式探针）：{"ok":true,"path":...,"count":N,"entries":[{"name","kind","size"}]}。
     * 用于证明「应用身份」能按路径读共享存储——adb shell 能读不代表应用能读。
     */
    public static native String listPath(String path);

    /**
     * 发送若干目录（M1）：target 形如 203.0.113.10:53318，peerFp 来自发现报文，
     * pathsJson 是 JSON 字符串数组（必须是目录），pairCode 可为空，
     * deviceName 传 Build.MODEL（Android 取不到 HOSTNAME，不传对端会显示 unknown）。
     * force=true 表示强制重传（v0.12）：忽略对端已有文件，同名旧文件由对端改名让位。
     * 返回 {"ok","txnId","files","bytes","verdict","selfFp"} 或 {"ok":false,"reason","selfFp"}。
     */
    public static native String sendJson(String target, String peerFp, String pathsJson,
                                         String pairCode, String identityDir, String deviceName,
                                         boolean force);

    /**
     * 当前发送进度：{"active","percent","bytesSent","totalBytes","blocksSent","totalBlocks","files","current"}。
     * 界面在发送期间轮询即可，不需要常驻轮询（发送结束后 active=false，快照保留）。
     */
    public static native String sendProgress();

    /**
     * 暂停 / 继续当前发送（v0.11）。暂停在块边界生效：不再向对端供块，
     * 连接与已传块都保留，继续后从断点接着发。
     */
    public static native void sendPause(boolean paused);

    /**
     * 取消当前发送（v0.11）。接收端会留下 `.tcpart`，下次同名发送可续传；
     * 发送端拿到的是 {"ok":false,"reason":"发送已取消"}。
     */
    public static native void sendCancel();

    /**
     * 听一轮发现公告（M2）：返回 {"ok":true,"count":N,"peers":[{"addr","name","platform","fingerprint"}]}。
     * 收组播需要宿主持 MulticastLock，JNI 层不做 Android API 调用。
     */
    public static native String discoverPeers(long timeoutMs, String identityDir);

    /**
     * 剪贴板拉取（v0.8）：返回 {"ok":true,"has":bool,"lamport":N} 或
     * {"ok":true,"has":true,"id","origin","size","sensitive","preview","text","lamport"}。
     * `since` 传上一次拿到的 lamport；服务端只回比它新的一条。
     */
    public static native String clipPullJson(String identityDir, String target, String peerFp,
                                             long since, String deviceName);

    /**
     * 剪贴板推送（v0.8）：把一段文本推到对端。超过 64 KiB 会被内核直接拒绝（内容不截断）。
     * 返回 {"ok":true,"id","size"} 或 {"ok":false,"reason"}。
     * deviceName 传 Build.MODEL：Android 没有 HOSTNAME，不传对端历史里会显示 unknown。
     */
    public static native String clipPushJson(String identityDir, String target, String peerFp,
                                             String text, boolean sensitive, String deviceName);
}
