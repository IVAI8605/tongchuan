package com.tongchuan;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.content.Context;
import android.database.Cursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;

import java.io.File;
import java.io.FileNotFoundException;
import java.net.URLConnection;

/**
 * 极简只读文件 Provider，只为了让系统能打开「接收目录里的文件」。
 *
 * 为什么不用 androidx 的 FileProvider：本项目刻意不引 AndroidX（零第三方依赖、离线可构建），
 * 而平台本身没有 FileProvider。这里只需要 openFile 一条路径，自己写比拉依赖更划算。
 * 安全上只放行当前接收目录内的文件，避免变成任意文件读取口子。
 */
public class FileServeProvider extends ContentProvider {

    public static Uri uriFor(Context ctx, File f) {
        // v1.4.1：路径走查询参数。原来用 appendPath 塞整条路径，
        // Builder 会把已经编码过的 `%5C` 再转义一次，openFile 解出来的路径多一层转义，
        // 于是「最近接收」点开永远 FileNotFound —— 用户看到的就是点了没反应。
        return new Uri.Builder()
                .scheme("content")
                .authority(ctx.getPackageName() + ".filesrv")
                .appendPath("file")
                .appendQueryParameter("p", f.getAbsolutePath())
                .build();
    }

    /** 取回真实路径；兼容旧格式（整条路径塞在一个 path segment 里）。 */
    private static String pathOf(Uri uri) {
        String p = uri.getQueryParameter("p");
        if (p != null && !p.isEmpty()) {
            return p;
        }
        String seg = uri.getLastPathSegment();
        return seg == null ? "" : Uri.decode(seg);
    }

    @Override
    public boolean onCreate() {
        return true;
    }

    @Override
    public ParcelFileDescriptor openFile(Uri uri, String mode) throws FileNotFoundException {
        if (!"r".equals(mode)) {
            throw new FileNotFoundException("只读");
        }
        String path = pathOf(uri);
        if (path.isEmpty()) {
            throw new FileNotFoundException("缺少路径");
        }
        File base = Dest.current(getContext()).getAbsoluteFile();
        File f = new File(path).getAbsoluteFile();
        if (!f.getPath().startsWith(base.getPath())) {
            // 只认接收目录内的文件；越界一律拒绝，不给出任何信息
            throw new FileNotFoundException("不在接收目录内");
        }
        return ParcelFileDescriptor.open(f, ParcelFileDescriptor.MODE_READ_ONLY);
    }

    @Override
    public String getType(Uri uri) {
        // 先按扩展名查系统 MIME 表，再退回 URLConnection 猜测；
        // 猜不到时仍给 `*/*`，让系统弹「选择打开方式」而不是直接报没有应用。
        String path = pathOf(uri);
        String name = new File(path).getName();
        int dot = name.lastIndexOf('.');
        if (dot > 0 && dot < name.length() - 1) {
            String mime = android.webkit.MimeTypeMap.getSingleton()
                    .getMimeTypeFromExtension(name.substring(dot + 1).toLowerCase());
            if (mime != null && !mime.isEmpty()) {
                return mime;
            }
        }
        String guess = URLConnection.guessContentTypeFromName(name);
        return guess == null ? "*/*" : guess;
    }

    @Override
    public Cursor query(Uri uri, String[] projection, String selection,
                        String[] selectionArgs, String sortOrder) {
        return null;
    }

    @Override
    public Uri insert(Uri uri, ContentValues values) {
        return null;
    }

    @Override
    public int delete(Uri uri, String selection, String[] selectionArgs) {
        return 0;
    }

    @Override
    public int update(Uri uri, ContentValues values, String selection, String[] selectionArgs) {
        return 0;
    }
}
