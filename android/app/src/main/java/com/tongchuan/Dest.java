package com.tongchuan;

import android.content.Context;
import android.content.SharedPreferences;
import android.net.Uri;
import android.os.Environment;
import android.provider.DocumentsContract;

import java.io.File;

/**
 * 接收落点。
 *
 * 为什么不按 SAF 的 fd 直接写：那需要把每个文件的句柄从 Java 回调给 Rust，
 * 还要自己做目录树映射，代码量翻倍且只在真机上才能验证。
 * 自用场景下用「SAF 选目录 + 所有文件访问权限」把 SAF 的树 URI 还原成真实路径，
 * Rust 侧仍然按路径写，既省事又能让用户自己决定存哪儿、且不会进相册。
 */
public final class Dest {

    static final String PREFS = "tongchuan";
    static final String KEY_PATH = "dest_path";

    private Dest() {
    }

    /** 当前落点：用户选过就用选的，没选过就用应用外部私有目录（免授权、卸载即清）。 */
    public static File current(Context ctx) {
        String p = prefs(ctx).getString(KEY_PATH, null);
        if (p != null) {
            File f = new File(p);
            if (f.isDirectory() || f.mkdirs()) {
                return f;
            }
        }
        return ReceiverService.receiveDir(ctx);
    }

    public static boolean hasCustom(Context ctx) {
        return prefs(ctx).getString(KEY_PATH, null) != null;
    }

    public static void clear(Context ctx) {
        prefs(ctx).edit().remove(KEY_PATH).apply();
    }

    public static void save(Context ctx, String path) {
        prefs(ctx).edit().putString(KEY_PATH, path).apply();
    }

    /**
     * 把 SAF 的树 URI 还原成真实路径。
     * 只处理主存储（primary），因为外部 provider（网盘、U 盘）没有稳定的真实路径，
     * 与其给出一个"看起来能选、实际写不进去"的选项，不如直接拒绝并提示。
     */
    public static String resolveTreePath(Uri uri) {
        try {
            String docId = DocumentsContract.getTreeDocumentId(uri);
            int colon = docId.indexOf(':');
            if (colon < 0) {
                return null;
            }
            String volume = docId.substring(0, colon);
            String rel = docId.substring(colon + 1);
            if (!"primary".equalsIgnoreCase(volume)) {
                return null;
            }
            File base = Environment.getExternalStorageDirectory();
            return rel.isEmpty() ? base.getAbsolutePath()
                    : new File(base, rel).getAbsolutePath();
        } catch (Exception e) {
            return null;
        }
    }

    /**
     * 把单个文件的 SAF URI 还原成真实路径（发送用）。
     * 与树 URI 同样的取舍：只认主存储，其它 provider 明确返回 null 由调用方提示。
     */
    public static String resolveDocumentPath(Uri uri) {
        try {
            String docId = DocumentsContract.getDocumentId(uri);
            // Downloads 等 provider 直接给真实路径：raw:/storage/emulated/0/Download/a.apk
            if (docId.startsWith("raw:")) {
                String raw = docId.substring(4);
                return new File(raw).isFile() ? raw : null;
            }
            int colon = docId.indexOf(':');
            if (colon < 0) {
                return null;
            }
            String volume = docId.substring(0, colon);
            String rel = docId.substring(colon + 1);
            if (!"primary".equalsIgnoreCase(volume)) {
                return null;
            }
            File base = Environment.getExternalStorageDirectory();
            return rel.isEmpty() ? null : new File(base, rel).getAbsolutePath();
        } catch (Exception e) {
            return null;
        }
    }

    /**
     * 通过已打开的 fd 反查真实路径。
     * Android 13 起系统相册选择器（media picker）给的是 `content://media/picker/...`，
     * 既没有 documentId、也查不到 `_data`，只有这条路能拿到文件实际落盘的位置。
     */
    public static String resolveViaFd(Context ctx, Uri uri) {
        try (android.os.ParcelFileDescriptor pfd =
                     ctx.getContentResolver().openFileDescriptor(uri, "r")) {
            if (pfd == null) {
                return null;
            }
            String link = android.system.Os.readlink("/proc/self/fd/" + pfd.getFd());
            if (link != null && new File(link).isFile()) {
                return link;
            }
        } catch (Exception ignored) {
            // 拿不到就交给下一条路；这里失败不代表文件不可读
        }
        return null;
    }

    /**
     * 最后兜底：把选中的内容复制一份到应用缓存再发。
     * 有些 provider 既没有真实路径也不暴露 fd 目标（例如第三方网盘），
     * 与其让「选择文件」永远报错，不如复制后发送——代价是占一份缓存，且落点文件夹会叫 tc-pick。
     */
    private static String copyToCache(Context ctx, Uri uri) {
        try {
            String name = queryDisplayName(ctx, uri);
            File dir = new File(ctx.getCacheDir(), "tc-pick");
            if (!dir.isDirectory() && !dir.mkdirs()) {
                return null;
            }
            File tmp = new File(dir, name + ".partial");
            File out = new File(dir, name);
            long copied = 0;
            try (java.io.InputStream in = ctx.getContentResolver().openInputStream(uri);
                 java.io.OutputStream os = new java.io.FileOutputStream(tmp)) {
                if (in == null) {
                    return null;
                }
                byte[] buf = new byte[64 * 1024];
                int n;
                while ((n = in.read(buf)) > 0) {
                    copied += n;
                    // 超过 512 MiB 不再复制：缓存盘会被撑爆，留给用户改用「选择文件夹」
                    if (copied > 512L * 1024 * 1024) {
                        return null;
                    }
                    os.write(buf, 0, n);
                }
            }
            // 复制完成后改名成正式文件名；先写 .partial 是为了让半截文件一眼可辨
            return tmp.renameTo(out) && out.isFile() ? out.getAbsolutePath() : null;
        } catch (Exception e) {
            return null;
        }
    }

    private static String queryDisplayName(Context ctx, Uri uri) {
        try (android.database.Cursor c = ctx.getContentResolver().query(
                uri, new String[]{android.provider.OpenableColumns.DISPLAY_NAME},
                null, null, null)) {
            if (c != null && c.moveToFirst() && c.getString(0) != null) {
                String name = c.getString(0).replace('/', '_').replace('\\', '_');
                if (!name.isEmpty()) {
                    return name;
                }
            }
        } catch (Exception ignored) {
        }
        // 取不到名字就用时间戳兜底，至少不会把两次选择写到同一个文件上
        return "picked-" + System.currentTimeMillis();
    }

    /**
     * 任意来源的选择结果 → 真实路径。
     * 优先按 externalstorage 的 docId 还原；相册/媒体类选择器给的是 MediaStore provider，
     * 改用它的 _data 列（在「所有文件访问」授权下可取到真实路径）。
     */
    public static String resolveAnyPath(Context ctx, Uri uri) {
        // 分享菜单里有些来源给的是 file://（实测 adb 与个别文件管理器都这样）。
        // 已拿到「所有文件访问」时直接按路径读，否则交给下面的 provider 分支去试。
        if ("file".equals(uri.getScheme())) {
            String p = uri.getPath();
            return p != null && new File(p).isFile() ? p : null;
        }
        String p = resolveDocumentPath(uri);
        if (p != null) {
            return p;
        }
        // Android 13+ 相册选择器（media picker）既没有 documentId 也查不到 _data，
        // 只能靠已打开的 fd 反查真实路径——这是「从内部存储选文件也失败」的主因
        p = resolveViaFd(ctx, uri);
        if (p != null) {
            return p;
        }
        try (android.database.Cursor c = ctx.getContentResolver().query(
                uri, new String[]{android.provider.MediaStore.MediaColumns.DATA},
                null, null, null)) {
            if (c != null && c.moveToFirst()) {
                String data = c.getString(0);
                if (data != null && new File(data).isFile()) {
                    return data;
                }
            }
        } catch (Exception ignored) {
            // 取不到就继续往下走，不要在这里猜路径
        }
        // 最后兜底：复制到应用缓存再发，保证「选择文件」这个入口永远能用
        return copyToCache(ctx, uri);
    }

    /** 「所有文件访问」是否已授权；没授权时只能写应用私有目录。 */
    public static boolean hasAllFilesAccess() {
        return Environment.isExternalStorageManager();
    }

    private static SharedPreferences prefs(Context ctx) {
        return ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
    }
}
