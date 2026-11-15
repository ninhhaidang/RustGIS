package org.rustgis.mobile;

/** Providers successfully registered for the current listener, confined to Android's main thread. */
final class LocationProviders {
    private final java.util.HashSet<String> requested = new java.util.HashSet<>();

    void add(String provider) {
        requested.add(provider);
    }

    void clear() {
        requested.clear();
    }

    boolean anyEnabled(java.util.function.Predicate<String> enabled) {
        for (String provider : requested) {
            if (enabled.test(provider)) {
                return true;
            }
        }
        return false;
    }
}
