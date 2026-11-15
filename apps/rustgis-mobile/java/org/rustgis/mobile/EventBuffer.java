package org.rustgis.mobile;

/** Ordered phone events, with bounded position retention and bounded work per Rust frame. */
final class EventBuffer {
    static final int PER_FRAME = 512;
    private final java.util.ArrayDeque<String> events = new java.util.ArrayDeque<>();
    private final int mostPositions;
    private int positions;
    private long dropped;

    EventBuffer(int mostPositions) {
        this.mostPositions = mostPositions;
    }

    void add(String event) {
        boolean position = event.startsWith("position\t");
        if (position && positions >= mostPositions) {
            // Only position fixes may be discarded: picker completions and failures must reach
            // Rust even when several days' positions filled the queue before them.
            java.util.Iterator<String> pending = events.iterator();
            while (pending.hasNext()) {
                if (pending.next().startsWith("position\t")) {
                    pending.remove();
                    positions--;
                    dropped++;
                    break;
                }
            }
        }
        events.add(event.replace('\n', ' ').replace('\r', ' '));
        if (position) {
            positions++;
        }
    }

    String take() {
        StringBuilder taken = new StringBuilder();
        int count = 0;
        if (dropped > 0) {
            taken.append("failed\t").append(dropped)
                    .append(" positions found while RustGIS was out of sight were dropped: more were waiting than it keeps.");
            dropped = 0;
            count++;
        }
        while (count < PER_FRAME && !events.isEmpty()) {
            String event = events.removeFirst();
            if (event.startsWith("position\t")) {
                positions--;
            }
            if (count++ > 0) {
                taken.append('\n');
            }
            taken.append(event);
        }
        return taken.toString();
    }

    boolean isEmpty() {
        return events.isEmpty() && dropped == 0;
    }
}
