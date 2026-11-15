package app.akergez.tressegui;

import android.content.ContentResolver;
import android.content.Context;
import android.content.UriPermission;
import android.database.Cursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.DocumentsContract;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.ByteArrayOutputStream;
import java.io.FileInputStream;
import java.io.FileNotFoundException;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayList;
import java.util.List;

/**
 * A picked folder, through the Storage Access Framework.
 *
 * Everything the vault needs is here: listing a directory, one document's
 * metadata, reading a byte range, writing a file (created when missing),
 * creating directories, removing a file or a tree of them, and opening the
 * repository lock. Paths are relative to the picked tree and use '/'.
 *
 * Every call is made by a Rust worker thread; none of these may run on the
 * UI thread.
 */
public final class SafBridge {
    private static final String[] LIST_COLUMNS = {
        DocumentsContract.Document.COLUMN_DOCUMENT_ID,
        DocumentsContract.Document.COLUMN_DISPLAY_NAME,
        DocumentsContract.Document.COLUMN_MIME_TYPE,
        DocumentsContract.Document.COLUMN_SIZE,
        DocumentsContract.Document.COLUMN_LAST_MODIFIED,
    };

    private static final String[] META_COLUMNS = {
        DocumentsContract.Document.COLUMN_MIME_TYPE,
        DocumentsContract.Document.COLUMN_SIZE,
        DocumentsContract.Document.COLUMN_LAST_MODIFIED,
    };

    /**
     * Document ids found during this run, by tree and path. A commit asks for
     * the same paths thousands of times; walking a path through every parent
     * on each call is what made a big folder crawl.
     */
    private static final java.util.concurrent.ConcurrentHashMap<String, String> IDS =
            new java.util.concurrent.ConcurrentHashMap<>();

    private static String key(Uri tree, String path) {
        return tree + "\u0000" + path;
    }

    private static void remember(Uri tree, String path, String id) {
        IDS.put(key(tree, path), id);
    }

    private static void forget(Uri tree, String path) {
        String exact = key(tree, path);
        String prefix = exact + "/";
        IDS.keySet().removeIf(cached -> cached.equals(exact) || cached.startsWith(prefix));
    }

    private static String childPath(String dir, String name) {
        return dir.isEmpty() ? name : dir + "/" + name;
    }

    private static Uri tree(String uri) {
        return Uri.parse(uri);
    }

    private static String name(String path) {
        return path.substring(path.lastIndexOf('/') + 1);
    }

    private static String parentPath(String path) {
        int slash = path.lastIndexOf('/');
        return slash < 0 ? "" : path.substring(0, slash);
    }

    /** Whether the granted access to this tree is still held, for reading and for writing. */
    public static boolean hasGrant(Context context, String uri) {
        Uri tree = tree(uri);
        for (UriPermission permission : context.getContentResolver().getPersistedUriPermissions()) {
            if (permission.getUri().equals(tree)
                    && permission.isReadPermission()
                    && permission.isWritePermission()) {
                return true;
            }
        }
        return false;
    }

    /** The direct children of `path`, as JSON: [{name, dir, len, mtime}]. */
    public static String list(Context context, String uri, String path) throws Exception {
        ContentResolver resolver = context.getContentResolver();
        Uri tree = tree(uri);
        String id = documentId(resolver, tree, path);
        if (id == null) return "[]";
        Uri children = DocumentsContract.buildChildDocumentsUriUsingTree(tree, id);
        JSONArray rows = new JSONArray();
        try (Cursor cursor = resolver.query(children, LIST_COLUMNS, null, null, null)) {
            if (cursor == null) return "[]";
            while (cursor.moveToNext()) {
                JSONObject row = new JSONObject();
                String childDoc = cursor.getString(0);
                String child = childPath(path, cursor.getString(1));
                remember(tree, child, childDoc);
                row.put("name", cursor.getString(1));
                String mime = cursor.isNull(2) ? "" : cursor.getString(2);
                row.put("dir", DocumentsContract.Document.MIME_TYPE_DIR.equals(mime));
                row.put("len", cursor.isNull(3) ? 0 : cursor.getLong(3));
                row.put("mtime", cursor.isNull(4) ? 0 : cursor.getLong(4));
                rows.put(row);
            }
        }
        return rows.toString();
    }

    /** One path's metadata, as JSON {dir, len, mtime}; null when there is nothing there. */
    public static String metadata(Context context, String uri, String path) throws Exception {
        ContentResolver resolver = context.getContentResolver();
        Uri tree = tree(uri);
        String id = documentId(resolver, tree, path);
        if (id == null) return null;
        Uri document = DocumentsContract.buildDocumentUriUsingTree(tree, id);
        try (Cursor cursor = resolver.query(document, META_COLUMNS, null, null, null)) {
            if (cursor == null || !cursor.moveToFirst()) return null;
            JSONObject meta = new JSONObject();
            String mime = cursor.isNull(0) ? "" : cursor.getString(0);
            meta.put("dir", DocumentsContract.Document.MIME_TYPE_DIR.equals(mime));
            meta.put("len", cursor.isNull(1) ? 0 : cursor.getLong(1));
            meta.put("mtime", cursor.isNull(2) ? 0 : cursor.getLong(2));
            return meta.toString();
        }
    }

    /** Up to `length` bytes of a file, from `offset`. */
    public static byte[] read(Context context, String uri, String path, long offset, int length)
            throws Exception {
        ContentResolver resolver = context.getContentResolver();
        Uri tree = tree(uri);
        String id = documentId(resolver, tree, path);
        if (id == null) throw new FileNotFoundException(path);
        Uri document = DocumentsContract.buildDocumentUriUsingTree(tree, id);
        try (ParcelFileDescriptor descriptor = resolver.openFileDescriptor(document, "r");
             InputStream input = descriptor == null ? null : new FileInputStream(descriptor.getFileDescriptor())) {
            if (input == null) throw new IOException("Could not read " + path);
            long skipped = 0;
            while (skipped < offset) {
                long step = input.skip(offset - skipped);
                if (step <= 0) break;
                skipped += step;
            }
            ByteArrayOutputStream output = new ByteArrayOutputStream();
            byte[] buffer = new byte[Math.max(1, Math.min(64 * 1024, length))];
            int remaining = length;
            while (remaining > 0) {
                int count = input.read(buffer, 0, Math.min(buffer.length, remaining));
                if (count < 0) break;
                output.write(buffer, 0, count);
                remaining -= count;
            }
            return output.toByteArray();
        }
    }

    /** Replaces a file's content, creating it (and its parents) when missing. */
    public static void write(Context context, String uri, String path, byte[] bytes)
            throws Exception {
        ContentResolver resolver = context.getContentResolver();
        Uri tree = tree(uri);
        mkdir(context, uri, parentPath(path));
        String id = documentId(resolver, tree, path);
        Uri document;
        if (id != null) {
            document = DocumentsContract.buildDocumentUriUsingTree(tree, id);
        } else {
            String parent = documentId(resolver, tree, parentPath(path));
            if (parent == null) throw new FileNotFoundException(parentPath(path));
            Uri parentUri = DocumentsContract.buildDocumentUriUsingTree(tree, parent);
            document = DocumentsContract.createDocument(resolver, parentUri,
                    "application/octet-stream", name(path));
            if (document == null) throw new IOException("Could not create " + path);
            remember(tree, path, DocumentsContract.getDocumentId(document));
        }
        try (OutputStream output = resolver.openOutputStream(document, "wt")) {
            if (output == null) throw new IOException("Could not write " + path);
            output.write(bytes);
        }
    }

    /** Creates a directory and its missing parents. */
    public static void mkdir(Context context, String uri, String path) throws Exception {
        if (path.isEmpty()) return;
        ContentResolver resolver = context.getContentResolver();
        Uri tree = tree(uri);
        String id = DocumentsContract.getTreeDocumentId(tree);
        StringBuilder built = new StringBuilder();
        for (String part : path.split("/")) {
            if (part.isEmpty()) continue;
            if (built.length() > 0) built.append('/');
            built.append(part);
            String child = childId(resolver, tree, id, part);
            if (child == null) {
                Uri parent = DocumentsContract.buildDocumentUriUsingTree(tree, id);
                Uri created = DocumentsContract.createDocument(resolver, parent,
                        DocumentsContract.Document.MIME_TYPE_DIR, part);
                if (created == null) throw new IOException("Could not create folder " + built);
                child = DocumentsContract.getDocumentId(created);
                remember(tree, built.toString(), child);
            }
            id = child;
        }
    }

    /** Removes a file, or a directory and everything under it. Nothing there is fine. */
    public static void remove(Context context, String uri, String path, boolean recursive)
            throws Exception {
        if (path.isEmpty()) return;
        ContentResolver resolver = context.getContentResolver();
        Uri tree = tree(uri);
        String id = documentId(resolver, tree, path);
        if (id == null) return;
        delete(context, tree, id, recursive);
        forget(tree, path);
    }

    /**
     * Opens the repository lock file and hands its descriptor to Rust, which
     * flocks it and closes it. The file is created when missing.
     */
    public static int openLock(Context context, String uri) throws Exception {
        ContentResolver resolver = context.getContentResolver();
        Uri tree = tree(uri);
        mkdir(context, uri, ".tresse");
        String path = ".tresse/lock.os";
        String id = documentId(resolver, tree, path);
        if (id == null) {
            String parent = documentId(resolver, tree, ".tresse");
            Uri parentUri = DocumentsContract.buildDocumentUriUsingTree(tree, parent);
            Uri created = DocumentsContract.createDocument(resolver, parentUri,
                    "application/octet-stream", "lock.os");
            if (created == null) throw new IOException("Could not create the repository lock");
            id = DocumentsContract.getDocumentId(created);
        }
        Uri document = DocumentsContract.buildDocumentUriUsingTree(tree, id);
        ParcelFileDescriptor descriptor = resolver.openFileDescriptor(document, "rw");
        if (descriptor == null) throw new IOException("Could not open the repository lock");
        return descriptor.detachFd();
    }

    private static void delete(Context context, Uri tree, String id, boolean recursive)
            throws Exception {
        ContentResolver resolver = context.getContentResolver();
        if (recursive) {
            Uri children = DocumentsContract.buildChildDocumentsUriUsingTree(tree, id);
            List<String> ids = new ArrayList<>();
            try (Cursor cursor = resolver.query(children,
                    new String[] { DocumentsContract.Document.COLUMN_DOCUMENT_ID }, null, null, null)) {
                if (cursor != null) {
                    while (cursor.moveToNext()) ids.add(cursor.getString(0));
                }
            }
            for (String child : ids) delete(context, tree, child, true);
        }
        DocumentsContract.deleteDocument(resolver, DocumentsContract.buildDocumentUriUsingTree(tree, id));
    }

    /** The document id of `path`, or null when nothing is there. */
    private static String documentId(ContentResolver resolver, Uri tree, String path) {
        String cached = IDS.get(key(tree, path));
        if (cached != null) return cached;
        String id = DocumentsContract.getTreeDocumentId(tree);
        if (path.isEmpty()) {
            remember(tree, path, id);
            return id;
        }
        for (String part : path.split("/")) {
            if (part.isEmpty()) continue;
            id = childId(resolver, tree, id, part);
            if (id == null) return null;
        }
        remember(tree, path, id);
        return id;
    }

    private static String childId(ContentResolver resolver, Uri tree, String parent, String name) {
        Uri children = DocumentsContract.buildChildDocumentsUriUsingTree(tree, parent);
        try (Cursor cursor = resolver.query(children, new String[] {
                DocumentsContract.Document.COLUMN_DOCUMENT_ID,
                DocumentsContract.Document.COLUMN_DISPLAY_NAME,
        }, null, null, null)) {
            if (cursor == null) return null;
            while (cursor.moveToNext()) {
                if (name.equals(cursor.getString(1))) return cursor.getString(0);
            }
        } catch (Exception ignored) {
            // A provider that refuses the query has no such child.
        }
        return null;
    }
}
