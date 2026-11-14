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
        return new Uri.Builder()
                .scheme("content")
                .authority(ctx.getPackageName() + ".filesrv")
                .appendPath(Uri.encode(f.getAbsolutePath()))
                .build();
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
        String path = Uri.decode(uri.getLastPathSegment());
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
        String path = Uri.decode(uri.getLastPathSegment());
        String guess = URLConnection.guessContentTypeFromName(path);
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
