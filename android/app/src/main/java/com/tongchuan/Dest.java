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
    /** 新默认落点目录名：放在公共 Download 下，用户用系统文件管理器就能直接找到收到的文件。 */
    static final String DEFAULT_DIR_NAME = "局域网通传";

    private Dest() {
    }

    /** 当前落点：用户选过就用选的，没选过就用默认（内置存储 / Download / 局域网通传）。 */
    public static File current(Context ctx) {
        String p = prefs(ctx).getString(KEY_PATH, null);
        if (p != null) {
            File f = new File(p);
            if (f.isDirectory() || f.mkdirs()) {
                return f;
            }
        }
        return defaultDir(ctx);
    }

    /**
     * 默认落点：内置存储 / Download / 局域网通传。
     *
     * 为什么不能直接 mkdirs 了事：targetSdk 35 起写公共 Download 必须持有「所有文件访问」
     * （MANAGE_EXTERNAL_STORAGE），否则 File API 直接 EACCES。没授权时退回 v0.20 之前的
     * 应用外部私有目录，保证「装完就能收」；用户授权后下一次刷新自动切到公共目录。
     * 这里不用 SAF：Rust 侧全程按真实路径写，SAF 的树 URI 还得额外做一层 fd 适配。
     */
    public static File defaultDir(Context ctx) {
        if (Environment.isExternalStorageManager()) {
            File pub = new File(Environment.getExternalStoragePublicDirectory(
                    Environment.DIRECTORY_DOWNLOADS), DEFAULT_DIR_NAME);
            if (pub.isDirectory() || pub.mkdirs()) {
                return pub;
            }
        }
        return legacyDir(ctx);
    }

    /**
     * v0.20 及之前的默认落点：应用外部私有目录。
     * 只作为无授权时的兜底与历史文件位置；旧文件不迁移、不删除，用户手动处理。
     */
    public static File legacyDir(Context ctx) {
        File dir = new File(ctx.getExternalFilesDir(null), "接收");
        if (!dir.isDirectory()) {
            //noinspection ResultOfMethodCallIgnored
            dir.mkdirs();
        }
        return dir;
    }

    /** 给人看的位置名：内置存储 / Download / 局域网通传；私有目录等其它位置直接显示绝对路径。 */
    public static String displayPath(File dir) {
        String abs = dir.getAbsolutePath();
        String base = Environment.getExternalStorageDirectory().getAbsolutePath();
        if (abs.startsWith(base + "/")) {
            // 逐段用 " / " 连接，读起来就是「内置存储 / Download / 局域网通传」，而不是一条斜杠串
            String[] parts = abs.substring(base.length() + 1).split("/");
            StringBuilder sb = new StringBuilder("内置存储");
            for (String p : parts) {
                if (!p.isEmpty()) {
                    sb.append(" / ").append(p);
                }
            }
            return sb.toString();
        }
        return abs;
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
     * 与其让「选择文件」永远报错，不如复制后发送——代价是占一份缓存。
     */
    private static String copyToCache(Context ctx, Uri uri) {
        try {
            String name = queryDisplayName(ctx, uri);
            // v0.13.1：缓存路径要带上来源文件夹名。发送端按「父目录名」生成落点根目录，
            // 全都直接放在 tc-pick 下会让首页摘要与对端落点都变成 tc-pick（真机实测踩到）。
            File dir = new File(new File(ctx.getCacheDir(), "tc-pick"), sourceFolderName(ctx, uri));
            if (!dir.isDirectory() && !dir.mkdirs()) {
                return null;
            }
            // v0.13.1：目标名不能用固定原名。重复选同一个文件时目标已存在，renameTo 会失败，
            // 旧实现直接返回 null，表现为「选中的某项无法定位」（真机实测踩到）。
            File out = uniqueFile(dir, name);
            File tmp = new File(dir, out.getName() + ".partial");
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

    /**
     * 相册选择器给的是 media documents provider 的 URI（documentId 形如 `image:12345`），
     * 它既没有可用的 `_data`，`relative_path` 也查不到。用里面的 id 去 MediaStore 查一次：
     * 在「所有文件访问」授权下能拿到真实路径，就不必退回「复制到缓存」（v0.13.1）。
     */
    private static String resolveMediaDocumentPath(Context ctx, Uri uri) {
        Uri item = mediaItemUri(uri);
        if (item == null) {
            return null;
        }
        try (android.database.Cursor c = ctx.getContentResolver().query(
                item, new String[]{android.provider.MediaStore.MediaColumns.DATA},
                null, null, null)) {
            if (c != null && c.moveToFirst()) {
                String p = c.getString(0);
                if (p != null && new File(p).isFile()) {
                    return p;
                }
            }
        } catch (Exception ignored) {
        }
        return null;
    }

    /** 把 `image:12345` / `video:12345` / `audio:12345` 还原成 MediaStore 里可查的条目 URI。 */
    private static Uri mediaItemUri(Uri uri) {
        try {
            String id = DocumentsContract.getDocumentId(uri);
            int colon = id.indexOf(':');
            if (colon <= 0) {
                return null;
            }
            String kind = id.substring(0, colon);
            long num = Long.parseLong(id.substring(colon + 1));
            if ("image".equals(kind)) {
                return android.content.ContentUris.withAppendedId(
                        android.provider.MediaStore.Images.Media.EXTERNAL_CONTENT_URI, num);
            }
            if ("video".equals(kind)) {
                return android.content.ContentUris.withAppendedId(
                        android.provider.MediaStore.Video.Media.EXTERNAL_CONTENT_URI, num);
            }
            if ("audio".equals(kind)) {
                return android.content.ContentUris.withAppendedId(
                        android.provider.MediaStore.Audio.Media.EXTERNAL_CONTENT_URI, num);
            }
        } catch (Exception ignored) {
            // 非媒体类 documentId（如 primary:DCIM/...）不在这里处理
        }
        return null;
    }

    /**
     * 来源文件夹名：缓存副本失去真实路径后，靠它恢复「用户从哪个文件夹选的」。
     * 优先用 MediaStore 的 relative_path（就是相册里看到的文件夹），
     * 再退回 SAF 的 documentId（形如 primary:DCIM/Camera/xxx.jpg）。
     */
    private static String sourceFolderName(Context ctx, Uri uri) {
        Uri item = mediaItemUri(uri);
        if (item != null) {
            try (android.database.Cursor c = ctx.getContentResolver().query(
                    item, new String[]{"relative_path"}, null, null, null)) {
                if (c != null && c.moveToFirst()) {
                    String seg = lastSegment(c.getString(0));
                    if (seg != null) {
                        return seg;
                    }
                }
            } catch (Exception ignored) {
            }
        }
        try (android.database.Cursor c = ctx.getContentResolver().query(
                uri, new String[]{"relative_path"}, null, null, null)) {
            if (c != null && c.moveToFirst()) {
                String seg = lastSegment(c.getString(0));
                if (seg != null) {
                    return seg;
                }
            }
        } catch (Exception ignored) {
        }
        try {
            String id = DocumentsContract.getDocumentId(uri);
            int colon = id.indexOf(':');
            String rel = colon >= 0 ? id.substring(colon + 1) : id;
            int slash = rel.lastIndexOf('/');
            if (slash > 0) {
                String seg = lastSegment(rel.substring(0, slash));
                if (seg != null) {
                    return seg;
                }
            }
        } catch (Exception ignored) {
            // 第三方 provider 的 documentId 可能没有目录信息，走下面的兜底名
        }
        return "已选文件";
    }

    /** 取路径最后一段非空目录名；空串或全是分隔符时返回 null。 */
    private static String lastSegment(String path) {
        if (path == null) {
            return null;
        }
        String[] parts = path.replace('\\', '/').split("/");
        for (int i = parts.length - 1; i >= 0; i--) {
            String s = parts[i].trim();
            if (!s.isEmpty()) {
                return s;
            }
        }
        return null;
    }

    /** 在同级目录里找一个没被占用的文件名：name.ext → name (1).ext → …，只新建不覆盖。 */
    private static File uniqueFile(File dir, String name) {
        File first = new File(dir, name);
        if (!first.exists()) {
            return first;
        }
        int dot = name.lastIndexOf('.');
        String base = dot > 0 ? name.substring(0, dot) : name;
        String ext = dot > 0 ? name.substring(dot) : "";
        for (int i = 1; i < 1000; i++) {
            File cand = new File(dir, base + " (" + i + ")" + ext);
            if (!cand.exists()) {
                return cand;
            }
        }
        // 极端情况（同名副本过千）用时间戳兜底，仍然不覆盖任何已有文件
        return new File(dir, base + "-" + System.currentTimeMillis() + ext);
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
        // v0.13.1：相册选择器（media documents）给的是 image:12345 这种 id，
        // 先按 id 去 MediaStore 还原真实路径；还原成功就不必复制到缓存，
        // 首页摘要与对端落点也就不会退化成缓存目录名或「已选文件」。
        p = resolveMediaDocumentPath(ctx, uri);
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
