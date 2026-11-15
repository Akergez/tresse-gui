package app.akergez.tressegui;

import android.app.Activity;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.provider.DocumentsContract;

import org.json.JSONObject;

import java.util.concurrent.CountDownLatch;

/**
 * Asks for a folder through the Storage Access Framework and keeps the grant.
 *
 * The picker is an activity of its own because the result comes back to the
 * activity that asked: the toolkit's NativeActivity does not forward it. A
 * request is parked for the Java side to answer, and the Rust worker that
 * asked waits on it — the same shape as the glue's file picker.
 */
public final class TreePicker extends Activity {
    private static final int PICK_TREE = 9041;

    private static final class Request {
        final CountDownLatch done = new CountDownLatch(1);
        String result;
        Exception error;

        synchronized void complete(String result, Exception error) {
            if (done.getCount() == 0) return;
            this.result = result;
            this.error = error;
            done.countDown();
        }
    }

    private static Request pending;
    private Request request;

    /**
     * Asks the person for a folder. Returns a JSON object {uri, name}, or null
     * when the person cancelled. Called by a Rust worker, never the UI thread.
     */
    public static String pick(Activity activity) throws Exception {
        Request current = new Request();
        synchronized (TreePicker.class) {
            if (pending != null) throw new IllegalStateException("A folder picker is already open");
            pending = current;
        }
        try {
            activity.runOnUiThread(() -> {
                try {
                    activity.startActivity(new Intent(activity, TreePicker.class));
                } catch (Exception error) {
                    current.complete(null, error);
                }
            });
            current.done.await();
            if (current.error != null) throw current.error;
            return current.result;
        } finally {
            synchronized (TreePicker.class) {
                if (pending == current) pending = null;
            }
        }
    }

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        synchronized (TreePicker.class) {
            request = pending;
        }
        // After process death the worker that asked is gone.
        if (request == null) {
            finish();
            return;
        }
        if (state != null) return;
        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE);
        intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION
                | Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION
                | Intent.FLAG_GRANT_PREFIX_URI_PERMISSION);
        try {
            startActivityForResult(intent, PICK_TREE);
        } catch (Exception error) {
            request.complete(null, error);
            finish();
        }
    }

    @Override
    protected void onActivityResult(int code, int result, Intent data) {
        super.onActivityResult(code, result, data);
        if (code != PICK_TREE || request == null) return;
        Uri uri = (result == RESULT_OK && data != null) ? data.getData() : null;
        if (uri == null) {
            request.complete(null, null);
            finish();
            return;
        }
        try {
            getContentResolver().takePersistableUriPermission(uri,
                    Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION);
        } catch (SecurityException ignored) {
            // A provider that cannot persist still works for this process.
        }
        try {
            String json = new JSONObject()
                    .put("uri", uri.toString())
                    .put("name", displayName(uri))
                    .toString();
            request.complete(json, null);
        } catch (Exception error) {
            request.complete(null, error);
        }
        finish();
    }

    private String displayName(Uri tree) {
        try {
            String id = DocumentsContract.getTreeDocumentId(tree);
            Uri document = DocumentsContract.buildDocumentUriUsingTree(tree, id);
            try (Cursor cursor = getContentResolver().query(document,
                    new String[] { DocumentsContract.Document.COLUMN_DISPLAY_NAME }, null, null, null)) {
                if (cursor != null && cursor.moveToFirst()) {
                    String name = cursor.getString(0);
                    if (name != null && !name.isEmpty()) return name;
                }
            }
            String segment = id.substring(id.lastIndexOf(':') + 1);
            return segment.isEmpty() ? "Folder" : segment;
        } catch (Exception ignored) {
            return "Folder";
        }
    }

    @Override
    protected void onDestroy() {
        if (isFinishing() && request != null) request.complete(null, null);
        super.onDestroy();
    }
}
