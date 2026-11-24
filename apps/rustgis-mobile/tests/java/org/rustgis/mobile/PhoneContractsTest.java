package org.rustgis.mobile;

/** Host-JVM regression checks; run with assertions enabled (no Android runtime needed). */
public final class PhoneContractsTest {
    public static void main(String[] args) {
        EventBuffer events = new EventBuffer(1200);
        events.add("picked\tdata\told\t/file");
        for (int i = 0; i < 2000; i++) {
            events.add("position\t" + i);
        }
        events.add("ended\tdxf:7");
        int fixes = 0;
        int notices = 0;
        int completions = 0;
        int previous = 799;
        while (!events.isEmpty()) {
            String[] batch = events.take().split("\n");
            assert batch.length <= EventBuffer.PER_FRAME;
            for (String event : batch) {
                if (event.startsWith("position\t")) {
                    int value = Integer.parseInt(event.substring(9));
                    assert value == ++previous : event;
                    fixes++;
                } else if (event.startsWith("failed\t")) {
                    assert event.startsWith("failed\t800 positions");
                    notices++;
                } else {
                    assert (completions == 0 && event.startsWith("picked\t"))
                            || (completions == 1 && event.equals("ended\tdxf:7"));
                    completions++;
                }
            }
        }
        assert fixes == 1200 && notices == 1 && completions == 2;
        assert events.take().isEmpty();
        LocationProviders providers = new LocationProviders();
        providers.add("network");
        // Approximate permission registered only network: enabled but unrequested GPS cannot
        // keep a dead network listener looking alive.
        assert !providers.anyEnabled(provider -> provider.equals("gps"));
        assert providers.anyEnabled(provider -> provider.equals("network"));
        providers.add("gps");
        assert providers.anyEnabled(provider -> provider.equals("gps"));
        providers.clear();
        assert !providers.anyEnabled(provider -> true);
        overwriteCutsALongerDocument();
        onlyAnOpenedDocumentsCopyIsSaidAfterARestart();
        System.out.println("Phone contract checks passed");
    }

    /**
     * After a restart, a staging folder is said only when its document was opened for writing;
     * a half copy, or a copy whose document was never opened, goes quietly, and one said already
     * isn't said again.
     */
    private static void onlyAnOpenedDocumentsCopyIsSaidAfterARestart() {
        try {
            java.io.File root = java.nio.file.Files.createTempDirectory("saved").toFile();
            java.io.File half = folder(root, "half", "site.rgis" + DocumentWrite.PART);
            java.io.File unopened = folder(root, "unopened", "site.rgis");
            java.io.File opened = folder(root, "opened", "site.rgis", DocumentWrite.OPENED);
            java.io.File said = folder(root, "said", "site.rgis", DocumentWrite.OPENED, DocumentWrite.REPORTED);
            // A document whose own name ends in .part: its whole copy is kept and said.
            java.io.File named = folder(root, "named", "site" + DocumentWrite.PART, DocumentWrite.OPENED);
            assert DocumentWrite.unfinished(half) == null && !half.exists();
            assert DocumentWrite.unfinished(unopened) == null && !unopened.exists();
            java.io.File copy = DocumentWrite.unfinished(opened);
            assert copy != null && copy.getName().equals("site.rgis") && copy.isFile();
            assert DocumentWrite.unfinished(said) == null && said.exists();
            java.io.File part = DocumentWrite.unfinished(named);
            assert part != null && part.getName().equals("site" + DocumentWrite.PART) && part.isFile();
        } catch (java.io.IOException e) {
            throw new AssertionError(e);
        }
    }

    /** A folder {@code name} in {@code root} holding empty files {@code files}. */
    private static java.io.File folder(java.io.File root, String name, String... files) throws java.io.IOException {
        java.io.File dir = new java.io.File(root, name);
        if (!dir.mkdirs()) {
            throw new java.io.IOException("no folder " + dir);
        }
        for (String file : files) {
            java.nio.file.Files.write(new java.io.File(dir, file).toPath(), new byte[] {1});
        }
        return dir;
    }

    /** A project written over a longer one leaves none of the old bytes after it. */
    private static void overwriteCutsALongerDocument() {
        try {
            java.io.File file = java.io.File.createTempFile("document", ".rgis");
            try {
                java.nio.file.Files.write(file.toPath(), "an older and longer project".getBytes("UTF-8"));
                try (java.io.RandomAccessFile open = new java.io.RandomAccessFile(file, "rw")) {
                    DocumentWrite.overwrite(open.getChannel(), "new".getBytes("UTF-8"));
                }
                String held = new String(java.nio.file.Files.readAllBytes(file.toPath()), "UTF-8");
                assert held.equals("new") : held;
                // Longer than before: all of it.
                try (java.io.RandomAccessFile open = new java.io.RandomAccessFile(file, "rw")) {
                    DocumentWrite.overwrite(open.getChannel(), "a newer and longer project, again".getBytes("UTF-8"));
                }
                held = new String(java.nio.file.Files.readAllBytes(file.toPath()), "UTF-8");
                assert held.equals("a newer and longer project, again") : held;
            } finally {
                file.delete();
            }
        } catch (java.io.IOException e) {
            throw new AssertionError(e);
        }
    }
}
