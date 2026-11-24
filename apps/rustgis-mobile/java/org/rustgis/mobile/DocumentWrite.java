package org.rustgis.mobile;

import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.channels.FileChannel;

/**
 * Writes a project over a document opened for reading and writing. Android's document API has no
 * way to replace a document whole, so the bytes are written from its start, the rest cut off, and
 * the result forced to storage; the caller keeps a complete copy of its own until this returns.
 */
final class DocumentWrite {
    /**
     * Why a write-back failed, as codes the Rust side words in the user's language (shell.rs,
     * WriteBackFailed); any other reason is the system's own text.
     */
    static final String BUSY = "code:busy";
    static final String STORAGE = "code:storage";
    static final String UNWRITABLE = "code:unwritable";
    static final String STALLED = "code:stalled";
    static final String SHORT = "code:short";
    static final String UNFINISHED = "code:unfinished";

    /** Beside a staged copy once its document is opened for writing (and emptied): from then on it may be damaged. */
    static final String OPENED = ".opened";
    /** Beside a kept copy whose failure has been said. */
    static final String REPORTED = ".reported";
    /** A staged copy being written, before it is given its name. */
    static final String PART = ".part";

    private DocumentWrite() {}

    /**
     * What an earlier process left in {@code dir}, one write-back's staging folder: the copy to say
     * (its document was opened for writing, so it may be damaged, and nothing has said so yet), or
     * null. A folder whose document was never opened is removed, with any half copy
     * ({@link #PART}) in it: the document is as it was.
     */
    static java.io.File unfinished(java.io.File dir) {
        java.io.File[] files = dir.listFiles();
        if (files == null || new java.io.File(dir, REPORTED).exists()) {
            return null;
        }
        boolean opened = new java.io.File(dir, OPENED).exists();
        java.io.File copy = null;
        for (java.io.File file : files) {
            String name = file.getName();
            // Opened, the copy was whole and named before the mark was made: whatever its name
            // (a document's own may end in .part), it is the copy.
            if (!opened) {
                file.delete();
            } else if (file.isFile() && !name.equals(OPENED)) {
                copy = file;
            }
        }
        if (!opened || copy == null) {
            new java.io.File(dir, OPENED).delete();
            dir.delete();
            return null;
        }
        return copy;
    }

    /**
     * Writes {@code bytes} over everything {@code channel} held, then forces it to storage: a
     * document longer before is cut to the new length.
     */
    static void overwrite(FileChannel channel, byte[] bytes) throws IOException {
        ByteBuffer buffer = ByteBuffer.wrap(bytes);
        long at = 0;
        while (buffer.hasRemaining()) {
            int n = channel.write(buffer, at);
            if (n <= 0) {
                throw new IOException(STALLED);
            }
            at += n;
        }
        channel.truncate(bytes.length);
        channel.force(true);
        long size = channel.size();
        if (size != bytes.length) {
            throw new IOException(SHORT);
        }
    }
}
