package org.rustgis.mobile;

import android.app.NativeActivity;
import android.content.ClipData;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.provider.OpenableColumns;
import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayList;

/**
 * The activity: Android's NativeActivity, plus what only Java can ask the system for. The Rust
 * side (src/phone.rs) calls the public methods and collects what happened with takeEvents: it
 * is never called from here, so the library exports nothing but its entry point.
 *
 * An event is one string, its fields separated by tabs:
 *   picked    purpose  name  path  [document]
 *                                    a chosen file, copied to the app's storage; a project file
 *                                    RustGIS may write back to also gives its document's address
 *   ended     purpose               a request whose every file failed to be copied
 *   cancelled                        a picker closed without a choice
 *   saved     name                   an export written where the user chose
 *   project-document     document    the document a project save is about to report written,
 *                                    which RustGIS may write back to later
 *   project-saved        name        a project written where the user chose, or written back
 *   write-back-failed    name  reason  [kept]
 *                                    a project not written back: the document is as it was, or,
 *                                    with kept, may be damaged and the project is kept there
 *   project-save-failed  reason      a project save that failed or was cancelled
 *   position  lon  lat  accuracy  time  elapsed   where the device is (accuracy in metres, or
 *                                        empty; time in milliseconds since 1970; elapsed in
 *                                        nanoseconds since boot, from getElapsedRealtimeNanos)
 *   stopped                          positioning ended without being asked to
 *   low-memory                       Android is short of memory
 *   photo     name  path             a photo the camera took, in the app's photos folder
 *   text      id  text               what a text field (enterText) holds now, its backslashes,
 *                                    tabs and line breaks written \\, \t, \n and \r
 *   text-closed  id  [reason]        a text field closed, or (with reason) couldn't be opened
 *   copy-failed  reason              copied text that couldn't be put on Android's clipboard
 *   failed    text                   what went wrong, to show the user
 */
public class MainActivity extends NativeActivity {
    private static final int PICK = 1;
    private static final int SAVE = 2;
    private static final int LOCATION = 3;
    private static final int NOTIFY = 4;
    private static final int PHOTO = 5;
    /**
     * The most events kept for the Rust side: two days of a position a second out of sight. The
     * Rust side keeps as many positions between frames (FIXES_KEPT), and each frame drains a bounded batch into its track.
     */
    private static final int MOST_EVENTS = 200_000;
    private static final int COPY_BUFFER = 1 << 16;
    /**
     * The least time between takeEvents' questions whether Android is low on memory: under the 5 s
     * an idle window waits between its calls (src/android.rs), so each of those asks.
     */
    private static final long MEMORY_LOOK_MILLIS = 4_000;
    /** Shared by every activity/copy worker for this process, including activity recreation. */
    private static final String COPY_OWNER = java.util.UUID.randomUUID().toString();

    /** Rust installs this token before inspecting unused document copies. */
    public String copyOwner() { return COPY_OWNER; }

    /** Oldest first; a deque, so the oldest goes at once when MOST_EVENTS wait. */
    private final EventBuffer events = new EventBuffer(MOST_EVENTS);
    /** Pickers shown and not yet answered in full (guarded by events). */
    private int asked;
    /** The window's insets as insets() gives them, written on the main thread. */
    private volatile String insets = "";
    private String purpose = "";
    /** Whether positions are wanted (the main thread's alone). */
    private boolean locating;
    /** Whether positions are wanted out of sight too, with TrackService running (the main thread's alone). */
    private boolean keepLocating;
    /**
     * Whether the Rust side wants positions out of sight (the main thread's alone): kept when
     * TrackService is refused, so it is tried again the next time the app comes into sight.
     */
    private boolean keepWanted;
    /** Why TrackService couldn't run, until takeEvents says so. */
    private static final java.util.concurrent.atomic.AtomicReference<String> refusal =
            new java.util.concurrent.atomic.AtomicReference<>();
    /** The callback Back comes to while the Rust side takes it (the main thread's alone). */
    private Object backCallback;
    /** Back came to backCallback and takeBack hasn't said so yet. */
    private final java.util.concurrent.atomic.AtomicBoolean back = new java.util.concurrent.atomic.AtomicBoolean();
    /** The window's surface while it has one (the main thread's alone): a frame is asked of it. */
    private android.view.SurfaceHolder surface;
    /** The activity, while it is created: TrackService's refusal reaches it at once. */
    private static volatile MainActivity current;
    /** The listener positions arrive on, while they are asked for. */
    private android.location.LocationListener positions;
    /** Providers this listener actually registered, on the main thread. */
    private final LocationProviders requestedProviders = new LocationProviders();
    /** The user is being asked for leave to use the location, and hasn't answered. */
    private boolean asking;
    /** The activity is in sight (between onStart and onStop). */
    private volatile boolean started;
    /** A picker for opening is shown and has not answered (written on the main thread). */
    private volatile boolean picking;
    /** The photo the camera app is writing, until it answers (the main thread's alone). */
    private File photo;
    private String saveName = "";
    private byte[] saveBytes;
    private boolean saveProject;
    /** A project is being written back to its document (set on the main thread, cleared by its worker). */
    private volatile boolean writing;
    /** When takeEvents last asked whether Android is low on memory, and its answer (its thread's alone). */
    private long memoryLooked;
    private boolean memoryLow;
    /** Whether the Rust side draws on a light ground (barsLight): the text field's colours follow it. */
    private volatile boolean light = true;
    /** The text field open over the app (enterText), while it is (the main thread's alone). */
    private android.app.Dialog entry;
    /**
     * The latest "text" event of a text field and that field's id, until takeEvents or the field's
     * closing hands it over (guarded by events): only the latest is kept, as each has all its text.
     */
    private String typedId;
    private String typed;

    @Override
    protected void onCreate(android.os.Bundle saved) {
        super.onCreate(saved);
        current = this;
        reportUnfinishedWriteBacks();
        // A photo being taken when Android ended the app (the camera app in front): its file.
        if (saved != null && saved.getString("photo") != null) {
            photo = new File(saved.getString("photo"));
            // The camera's answer is still to come: answered() balances this.
            synchronized (events) {
                asked++;
            }
        }
        // The insets are read where Android hands them over, on the main thread, and kept for
        // the Rust side to ask for from its own.
        getWindow().getDecorView().setOnApplyWindowInsetsListener((view, given) -> {
            insets = describe(given);
            return view.onApplyWindowInsets(given);
        });
        barsLight(true);
    }

    /**
     * Draws the system bars' icons dark, for a light ground under them, or light, for a dark one:
     * the Rust side says which whenever its theme changes.
     */
    public void barsLight(final boolean light) {
        this.light = light;
        runOnUiThread(() -> {
            if (android.os.Build.VERSION.SDK_INT >= 30) {
                android.view.WindowInsetsController bars = getWindow().getInsetsController();
                if (bars != null) {
                    int both = android.view.WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS
                            | android.view.WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS;
                    bars.setSystemBarsAppearance(light ? both : 0, both);
                }
            } else {
                int both = android.view.View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR | android.view.View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR;
                android.view.View view = getWindow().getDecorView();
                int flags = view.getSystemUiVisibility();
                view.setSystemUiVisibility(light ? flags | both : flags & ~both);
            }
        });
    }

    /**
     * Takes Back from the system while the Rust side has a sheet it lowers (on), or gives it
     * back, so Back does what the system does with it. Needed from Android 16, which sends an
     * app targeting it no Back key; older ones send the key, and the Rust side reads that.
     */
    public void takesBack(final boolean on) {
        if (android.os.Build.VERSION.SDK_INT < 36) {
            return;
        }
        runOnUiThread(() -> {
            if (on && backCallback == null) {
                android.window.OnBackInvokedCallback callback = () -> {
                    back.set(true);
                    // The Rust side draws only when something happens: ask it for a frame.
                    if (surface != null) {
                        surfaceRedrawNeeded(surface);
                    }
                };
                getOnBackInvokedDispatcher().registerOnBackInvokedCallback(
                        android.window.OnBackInvokedDispatcher.PRIORITY_DEFAULT, callback);
                backCallback = callback;
            } else if (!on && backCallback != null) {
                getOnBackInvokedDispatcher()
                        .unregisterOnBackInvokedCallback((android.window.OnBackInvokedCallback) backCallback);
                backCallback = null;
                // One not read yet lowered nothing: it isn't kept for the next sheet.
                back.set(false);
            }
        });
    }

    /** Whether Back came since this was last asked. */
    public boolean takeBack() {
        return back.getAndSet(false);
    }

    @Override
    public void surfaceCreated(android.view.SurfaceHolder holder) {
        super.surfaceCreated(holder);
        surface = holder;
    }

    @Override
    public void surfaceDestroyed(android.view.SurfaceHolder holder) {
        surface = null;
        super.surfaceDestroyed(holder);
    }

    /**
     * Starts or stops reporting the device's position: each fix arrives as a "position" event
     * (longitude, latitude, accuracy in metres). Starting asks for the location permission where
     * it isn't given yet; a refusal arrives as a "failed" event.
     */
    public void locate(final boolean on) {
        runOnUiThread(() -> {
            locating = on;
            if (!on) {
                stopLocating();
            } else if (allowed(android.Manifest.permission.ACCESS_FINE_LOCATION)
                    || allowed(android.Manifest.permission.ACCESS_COARSE_LOCATION)) {
                startLocating();
            } else {
                asking = true;
                requestPermissions(new String[] {android.Manifest.permission.ACCESS_FINE_LOCATION,
                        android.Manifest.permission.ACCESS_COARSE_LOCATION}, LOCATION);
            }
        });
    }

    @Override
    public void onRequestPermissionsResult(int request, String[] permissions, int[] results) {
        super.onRequestPermissionsResult(request, permissions, results);
        // Leave to show notifications only shows TrackService's notice: it runs either way. A
        // notice posted before the leave was given isn't shown, so the service posts it again.
        if (request == NOTIFY) {
            if (keepLocating && results.length > 0 && results[0] == android.content.pm.PackageManager.PERMISSION_GRANTED) {
                try {
                    startForegroundService(new Intent(this, TrackService.class));
                } catch (RuntimeException e) {
                    // Not started again: positions aren't kept out of sight after all.
                    keepLocating = false;
                    add("failed\tPositions can't be kept while RustGIS is out of sight: " + e.getMessage());
                }
            }
            return;
        }
        if (request != LOCATION) {
            return;
        }
        asking = false;
        if (!locating) {
            return;
        }
        boolean given = false;
        for (int result : results) {
            given |= result == android.content.pm.PackageManager.PERMISSION_GRANTED;
        }
        if (given) {
            // Out of sight, the start is left to onStart: positions would only pile up.
            if (started) {
                startLocating();
            }
            // A track asked for positions out of sight before the leave was given.
            if (keepLocating) {
                startTrackService();
            }
        } else {
            stopped("RustGIS wasn't given leave to use this device's location: its position can't be shown.");
        }
    }

    private boolean allowed(String permission) {
        return checkSelfPermission(permission) == android.content.pm.PackageManager.PERMISSION_GRANTED;
    }

    /**
     * Positioning has ended without the Rust side asking: why is said ("failed"), and "stopped"
     * tells it so, so the interface doesn't go on offering to hide a position that isn't coming.
     */
    private void stopped(String why) {
        locating = false;
        // Out of sight too: nothing is kept now, so the service and its notice go.
        if (keepLocating) {
            keepLocating = false;
            stopService(new Intent(this, TrackService.class));
        }
        stopLocating();
        add("failed\t" + why);
        add("stopped");
    }

    /**
     * Positions are not asked for while the app is out of sight, where they would only pile up,
     * unless a track wants them (keepLocating): TrackService then keeps the app in the foreground
     * for Android, and they wait in the events.
     */
    @Override
    protected void onStop() {
        started = false;
        if (!keepLocating) {
            stopLocating();
        }
        super.onStop();
    }

    /**
     * Keeps positions coming while the app is out of sight (on), for a track, or lets them stop
     * there again (off). On starts TrackService, a foreground service with its notification, and
     * asks for leave to show notifications where Android wants it (13 and later; refused, the
     * service still runs, its notice unseen). Called while the app is in sight.
     */
    public void keepLocating(final boolean on) {
        runOnUiThread(() -> {
            keepLocating = on;
            keepWanted = on;
            Intent service = new Intent(this, TrackService.class);
            if (!on) {
                stopService(service);
                if (!started) {
                    stopLocating();
                }
                return;
            }
            // A location service needs the location leave first (Android 10 and later refuse it
            // otherwise): while the user is still being asked, the answer starts it.
            if (allowed(android.Manifest.permission.ACCESS_FINE_LOCATION)
                    || allowed(android.Manifest.permission.ACCESS_COARSE_LOCATION)) {
                startTrackService();
            }
        });
    }

    /** Starts TrackService for keepLocating, asking for leave to show its notice where needed. */
    private void startTrackService() {
        if (android.os.Build.VERSION.SDK_INT >= 33 && !allowed("android.permission.POST_NOTIFICATIONS")) {
            requestPermissions(new String[] {"android.permission.POST_NOTIFICATIONS"}, NOTIFY);
        }
        try {
            startForegroundService(new Intent(this, TrackService.class));
        } catch (RuntimeException e) {
            keepLocating = false;
            add("failed\tPositions can't be kept while RustGIS is out of sight: " + e.getMessage());
        }
    }

    /**
     * TrackService was refused by Android: takeEvents says why, and the activity stops keeping
     * positions out of sight now, not at the next frame (an app out of sight draws none).
     */
    static void serviceRefused(String why) {
        refusal.set(why);
        MainActivity activity = current;
        if (activity != null) {
            activity.keepLocatingRefused();
        }
    }

    /** Back in sight: positions are asked for again, if they were wanted and are still allowed. */
    @Override
    protected void onStart() {
        super.onStart();
        started = true;
        // While the user is still being asked for leave, its answer decides, not this.
        if (locating && !asking) {
            if (allowed(android.Manifest.permission.ACCESS_FINE_LOCATION)
                    || allowed(android.Manifest.permission.ACCESS_COARSE_LOCATION)) {
                startLocating();
                // A track still wants positions out of sight that TrackService was refused for
                // (started out of sight, say): tried again now, in sight.
                if (keepWanted && !keepLocating) {
                    keepLocating = true;
                    startTrackService();
                }
            } else {
                stopped("RustGIS is no longer given leave to use this device's location.");
            }
        }
    }

    /** Asks every provider the device has (satellites, the network) for positions, about one a second. */
    private void startLocating() {
        android.location.LocationManager manager = (android.location.LocationManager) getSystemService(LOCATION_SERVICE);
        if (manager == null) {
            stopped("This device has no location service.");
            return;
        }
        stopLocating();
        // Every callback is written out: before Android 11 none has a default, and a provider
        // switched off would call one that isn't there.
        positions = new android.location.LocationListener() {
            @Override
            public void onLocationChanged(android.location.Location fix) {
                add("position\t" + fix.getLongitude() + "\t" + fix.getLatitude() + "\t"
                        + (fix.hasAccuracy() ? Float.toString(fix.getAccuracy()) : "") + "\t" + fix.getTime()
                        + "\t" + fix.getElapsedRealtimeNanos());
            }

            @Override
            public void onProviderEnabled(String provider) {}

            /** Location switched off while positions are wanted: with no provider left, say so. */
            @Override
            public void onProviderDisabled(String provider) {
                android.location.LocationManager manager =
                        (android.location.LocationManager) getSystemService(LOCATION_SERVICE);
                if (!locating || manager == null) {
                    return;
                }
                if (requestedProviders.anyEnabled(other -> manager.getAllProviders().contains(other)
                        && manager.isProviderEnabled(other))) {
                    return;
                }
                stopped("This device's location was turned off in its settings.");
            }

            @Override
            public void onStatusChanged(String provider, int status, android.os.Bundle extras) {}
        };
        int asked = 0;
        for (String provider : new String[] {android.location.LocationManager.GPS_PROVIDER,
                android.location.LocationManager.NETWORK_PROVIDER}) {
            try {
                // Satellites need the precise permission: with the approximate one alone, the
                // network's positions are the ones to ask for.
                boolean satellites = provider.equals(android.location.LocationManager.GPS_PROVIDER);
                // One switched off would be asked without complaint and never answer.
                if (manager.getAllProviders().contains(provider) && manager.isProviderEnabled(provider)
                        && (!satellites || allowed(android.Manifest.permission.ACCESS_FINE_LOCATION))) {
                    manager.requestLocationUpdates(provider, 1000, 0, positions, getMainLooper());
                    requestedProviders.add(provider);
                    asked++;
                }
            } catch (SecurityException | IllegalArgumentException e) {
                add("failed\tThis device's location can't be read (" + provider + "): " + e.getMessage());
            }
        }
        if (asked == 0) {
            stopped("This device's position can't be found: location is turned off in its settings, or it has no way to find one.");
        }
    }

    private void stopLocating() {
        android.location.LocationManager manager = (android.location.LocationManager) getSystemService(LOCATION_SERVICE);
        if (manager != null && positions != null) {
            manager.removeUpdates(positions);
        }
        positions = null;
        requestedProviders.clear();
    }

    @Override
    protected void onDestroy() {
        if (current == this) {
            current = null;
        }
        if (entry != null) {
            entry.dismiss();
        }
        stopLocating();
        stopService(new Intent(this, TrackService.class));
        super.onDestroy();
    }

    /** Keeps the photo being taken's file, should Android end the app while the camera is up. */
    @Override
    protected void onSaveInstanceState(android.os.Bundle out) {
        super.onSaveInstanceState(out);
        if (photo != null) {
            out.putString("photo", photo.getAbsolutePath());
        }
    }

    /**
     * Asks the camera app for a photo, written to the app's photos folder through PhotoProvider:
     * a "photo" event follows, or "cancelled". The device's own camera app takes it, so RustGIS
     * needs no camera permission.
     */
    public void takePhoto() {
        runOnUiThread(() -> {
            if (photo != null) {
                add("failed\tA photo is already being taken. Take another when it is done.");
                return;
            }
            String stamp = new java.text.SimpleDateFormat("yyyyMMdd-HHmmss", java.util.Locale.ROOT).format(new java.util.Date());
            File file = new File(PhotoProvider.folder(this), "photo-" + stamp + ".jpg");
            for (int n = 2; file.exists(); n++) {
                file = new File(PhotoProvider.folder(this), "photo-" + stamp + "-" + n + ".jpg");
            }
            Uri uri = Uri.parse("content://" + PhotoProvider.AUTHORITY + "/" + file.getName());
            Intent intent = new Intent(android.provider.MediaStore.ACTION_IMAGE_CAPTURE);
            intent.putExtra(android.provider.MediaStore.EXTRA_OUTPUT, uri);
            // The leave to write goes with the address, to whichever camera app answers.
            intent.setClipData(ClipData.newRawUri("photo", uri));
            intent.addFlags(Intent.FLAG_GRANT_WRITE_URI_PERMISSION | Intent.FLAG_GRANT_READ_URI_PERMISSION);
            photo = file;
            try {
                startActivityForResult(intent, PHOTO);
                synchronized (events) {
                    asked++;
                }
            } catch (RuntimeException e) {
                photo = null;
                add("failed\tThis device has no camera app: " + e.getMessage());
            }
        });
    }

    /**
     * Opens a text field of Android's for a field the Rust side draws, docked above the soft
     * keyboard, in a window of its own: NativeActivity hands its window's touches and keys to the
     * Rust side, so a field there would get none. It gives Android's typing: paste, suggestions,
     * glide typing, dictation, and a password manager for a "secret". What it holds is sent as a
     * "text" event each time it changes, and "text-closed" when it closes (Done, Back, or a tap
     * beside it, which goes no further). One opened while another is open replaces it. The kind
     * is "plain", "lines", "secret" or "number"; done is the label of its button.
     */
    public void enterText(final String id, final String value, final String label, final String kind, final String done) {
        // Counted until it closes: the Rust side keeps looking for what is typed meanwhile.
        synchronized (events) {
            asked++;
        }
        runOnUiThread(() -> {
            // Its own close is said by its listener.
            if (entry != null) {
                entry.dismiss();
            }
            try {
                entry = textEntry(id, value, label, kind, done);
                entry.show();
            } catch (RuntimeException e) {
                entry = null;
                String why = e.getMessage();
                add("text-closed\t" + id + "\t" + oneField(why == null || why.isEmpty() ? e.getClass().getSimpleName() : why));
                answered();
            }
        });
    }

    /** The window of a text field (enterText), not yet shown. */
    private android.app.Dialog textEntry(final String id, String value, String label, String kind, String done) {
        final android.app.Dialog dialog = new android.app.Dialog(this, light
                ? android.R.style.Theme_DeviceDefault_Light_Dialog_NoActionBar
                : android.R.style.Theme_DeviceDefault_Dialog_NoActionBar);
        android.content.Context themed = dialog.getContext();
        final int pad = Math.round(12 * getResources().getDisplayMetrics().density);
        android.widget.LinearLayout panel = new android.widget.LinearLayout(themed);
        panel.setOrientation(android.widget.LinearLayout.VERTICAL);
        panel.setPadding(pad, pad, pad, pad);
        android.widget.TextView title = new android.widget.TextView(themed);
        title.setText(label);
        panel.addView(title);
        android.widget.LinearLayout row = new android.widget.LinearLayout(themed);
        row.setOrientation(android.widget.LinearLayout.HORIZONTAL);
        row.setGravity(android.view.Gravity.CENTER_VERTICAL);
        android.widget.EditText field = new android.widget.EditText(themed);
        int text = android.text.InputType.TYPE_CLASS_TEXT;
        switch (kind) {
            case "secret":
                field.setInputType(text | android.text.InputType.TYPE_TEXT_VARIATION_PASSWORD);
                field.setAutofillHints(android.view.View.AUTOFILL_HINT_PASSWORD);
                field.setImportantForAutofill(android.view.View.IMPORTANT_FOR_AUTOFILL_YES);
                break;
            case "number":
                // The keyboard for numbers, without the filter that would come with it: it drops
                // what isn't a digit, sign or point (the "e" of a pasted 1e3 would leave 13). The
                // Rust side reads the number and refuses one it can't.
                field.setInputType(text);
                field.setRawInputType(android.text.InputType.TYPE_CLASS_NUMBER
                        | android.text.InputType.TYPE_NUMBER_FLAG_DECIMAL | android.text.InputType.TYPE_NUMBER_FLAG_SIGNED);
                break;
            case "lines":
                field.setInputType(text | android.text.InputType.TYPE_TEXT_FLAG_MULTI_LINE
                        | android.text.InputType.TYPE_TEXT_FLAG_CAP_SENTENCES);
                field.setMaxLines(6);
                break;
            default:
                field.setInputType(text | android.text.InputType.TYPE_TEXT_FLAG_CAP_SENTENCES);
        }
        // A form's field isn't a sign-in: the password manager is offered for a secret alone.
        if (!kind.equals("secret")) {
            field.setImportantForAutofill(android.view.View.IMPORTANT_FOR_AUTOFILL_NO);
        }
        field.setText(value);
        field.setSelection(field.length());
        // The keyboard keeps to its own strip in landscape too, with the form in sight above it.
        field.setImeOptions(android.view.inputmethod.EditorInfo.IME_ACTION_DONE
                | android.view.inputmethod.EditorInfo.IME_FLAG_NO_FULLSCREEN);
        field.setOnEditorActionListener((view, action, key) -> {
            if (action == android.view.inputmethod.EditorInfo.IME_ACTION_DONE) {
                dialog.dismiss();
                return true;
            }
            return false;
        });
        field.addTextChangedListener(new android.text.TextWatcher() {
            @Override
            public void beforeTextChanged(CharSequence was, int start, int count, int after) {}

            @Override
            public void onTextChanged(CharSequence now, int start, int before, int count) {}

            @Override
            public void afterTextChanged(android.text.Editable now) {
                typed(id, now.toString());
            }
        });
        row.addView(field, new android.widget.LinearLayout.LayoutParams(0, android.view.ViewGroup.LayoutParams.WRAP_CONTENT, 1f));
        android.widget.Button finish = new android.widget.Button(themed);
        finish.setText(done);
        finish.setOnClickListener(view -> dialog.dismiss());
        row.addView(finish);
        panel.addView(row);
        dialog.setContentView(panel);
        dialog.setCanceledOnTouchOutside(true);
        dialog.setOnDismissListener(closed -> {
            if (entry == dialog) {
                entry = null;
            }
            synchronized (events) {
                if (typed != null && id.equals(typedId)) {
                    events.add(typed);
                    typed = null;
                }
                events.add("text-closed\t" + id);
                asked--;
            }
        });
        android.view.Window window = dialog.getWindow();
        if (window != null) {
            window.setGravity(android.view.Gravity.BOTTOM);
            window.setLayout(android.view.ViewGroup.LayoutParams.MATCH_PARENT, android.view.ViewGroup.LayoutParams.WRAP_CONTENT);
            // The form stays in sight behind it, the value it shows changing as it is typed.
            window.clearFlags(android.view.WindowManager.LayoutParams.FLAG_DIM_BEHIND);
            window.setSoftInputMode(android.view.WindowManager.LayoutParams.SOFT_INPUT_STATE_ALWAYS_VISIBLE
                    | android.view.WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE);
            android.content.res.TypedArray colours = themed.obtainStyledAttributes(new int[] {android.R.attr.colorBackground});
            window.setBackgroundDrawable(new android.graphics.drawable.ColorDrawable(
                    colours.getColor(0, light ? android.graphics.Color.WHITE : android.graphics.Color.BLACK)));
            colours.recycle();
        }
        // Drawn edge to edge (Android 15 and later), it keeps above the keyboard and the
        // navigation bar itself.
        if (android.os.Build.VERSION.SDK_INT >= 30) {
            panel.setOnApplyWindowInsetsListener((view, insets) -> {
                android.graphics.Insets room = insets.getInsets(android.view.WindowInsets.Type.navigationBars()
                        | android.view.WindowInsets.Type.displayCutout() | android.view.WindowInsets.Type.ime());
                view.setPadding(pad + room.left, pad, pad + room.right, pad + room.bottom);
                return insets;
            });
        }
        field.requestFocus();
        return dialog;
    }

    /** Keeps what the text field {@code id} holds now as its "text" event, the one before it of another field handed over first. */
    private void typed(String id, String text) {
        StringBuilder line = new StringBuilder("text\t").append(id).append('\t');
        for (int i = 0; i < text.length(); i++) {
            char c = text.charAt(i);
            switch (c) {
                case '\\': line.append("\\\\"); break;
                case '\t': line.append("\\t"); break;
                case '\n': line.append("\\n"); break;
                case '\r': line.append("\\r"); break;
                default: line.append(c);
            }
        }
        synchronized (events) {
            if (typed != null && !id.equals(typedId)) {
                events.add(typed);
            }
            typedId = id;
            typed = line.toString();
        }
    }

    /** Puts text copied in the Rust side's window on Android's clipboard, for other apps. */
    public void copyText(final String text) {
        // Counted until it is done: the Rust side keeps looking until a failure has reached it.
        synchronized (events) {
            asked++;
        }
        runOnUiThread(() -> {
            try {
                android.content.ClipboardManager clipboard = (android.content.ClipboardManager) getSystemService(CLIPBOARD_SERVICE);
                if (clipboard == null) {
                    add("copy-failed\tthis device has no clipboard service");
                } else {
                    clipboard.setPrimaryClip(ClipData.newPlainText("RustGIS", text));
                }
            } catch (RuntimeException e) {
                // Too large for Android to pass on, say: the Rust side's own clipboard still has it.
                add("copy-failed\t" + oneField(e.getMessage() == null ? e.getClass().getSimpleName() : e.getMessage()));
            } finally {
                answered();
            }
        });
    }

    /** Asks the user for files with the system's picker; each arrives as a "picked" event. */
    public void pick(final String purpose) {
        runOnUiThread(() -> {
            if (picking) {
                add("failed\tAnother file is already being chosen. Choose files again when it is done.");
                return;
            }
            picking = true;
            this.purpose = purpose;
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            intent.setType("*/*");
            // Several files only where the front end takes several, as in the browser: two
            // projects would replace one another, and a dialog's options go with one file.
            intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, purpose.equals("data") || purpose.equals("shapefile"));
            // A project is opened for writing too, for this session: Save writes it back.
            if (purpose.equals("project")) {
                intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION);
            }
            if (!start(intent, PICK)) {
                picking = false;
            }
        });
    }

    /** Asks where to write a file. Project saves return a distinct result to the save prompt. */
    public void save(final String name, final byte[] bytes, final boolean project) {
        runOnUiThread(() -> {
            if (saveBytes != null || picking || writing) {
                // A project's refusal is a code the Rust side words; an export's is as it was.
                if (project) {
                    add(writeBackFailed(name, new java.io.IOException(DocumentWrite.BUSY), null));
                } else {
                    saveFailed(false, name, writing ? "another file is being saved." : "another file is being chosen.");
                }
                return;
            }
            saveName = name;
            saveBytes = bytes;
            saveProject = project;
            Intent intent = new Intent(Intent.ACTION_CREATE_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            // The file's own type where Android knows its extension: some providers name the
            // file by it.
            int dot = name.lastIndexOf('.');
            String type = dot < 0 ? null
                    : android.webkit.MimeTypeMap.getSingleton().getMimeTypeFromExtension(name.substring(dot + 1).toLowerCase());
            intent.setType(type == null ? "application/octet-stream" : type);
            intent.putExtra(Intent.EXTRA_TITLE, name);
            if (!start(intent, SAVE)) {
                saveBytes = null;
            }
        });
    }

    /**
     * Writes a project back to the document it was opened from or last saved as, with no picker.
     * A complete copy is written to the app's storage first: if the document can't be opened for
     * writing, it is left as it was and the copy goes; if writing it fails partway, it may be
     * damaged, and the copy is kept and named. The result is a project-saved or write-back-failed
     * event (with the code DocumentWrite.BUSY when another save is under way).
     */
    public void writeBack(final String document, final String name, final byte[] bytes) {
        // Counted before anything is posted: the Rust side keeps looking until the answer comes,
        // as no picker will wake it.
        synchronized (events) {
            asked++;
        }
        runOnUiThread(() -> {
            if (saveBytes != null || picking || writing) {
                add(writeBackFailed(name, new java.io.IOException(DocumentWrite.BUSY), null));
                answered();
                return;
            }
            writing = true;
            try {
                new Thread(() -> {
                    String result;
                    // The copy kept when the document may be damaged, said with the result.
                    File[] kept = new File[1];
                    try {
                        result = writeBackNow(Uri.parse(document), name, bytes, kept);
                    } catch (Throwable e) {
                        result = writeBackFailed(name, e, null);
                    }
                    // Free for the next save before it is told this one is done.
                    writing = false;
                    addKept(result, kept[0]);
                    answered();
                }).start();
            } catch (Throwable e) {
                writing = false;
                add(writeBackFailed(name, e, null));
                answered();
            }
        });
    }

    /** Writes the project back; returns the event that says how it went. */
    private String writeBackNow(Uri uri, String name, byte[] bytes, File[] kept) {
        // The complete copy, kept until the document holds the project.
        File copy;
        try {
            File dir = new File(new File(getFilesDir(), "saved"), Long.toString(System.nanoTime()));
            if (!dir.isDirectory() && !dir.mkdirs()) {
                throw new java.io.IOException(DocumentWrite.STORAGE);
            }
            copy = new File(dir, displayName(uri));
        } catch (Throwable e) {
            return writeBackFailed(name, e, null);
        }
        // Written under a name of its own and given its name once whole: a later start never takes a
        // half-written copy for the project as saved.
        File part = new File(copy.getParentFile(), copy.getName() + DocumentWrite.PART);
        try {
            try (FileOutputStream out = new FileOutputStream(part)) {
                out.write(bytes);
                out.getFD().sync();
            }
            if (!part.renameTo(copy)) {
                throw new java.io.IOException(DocumentWrite.STORAGE);
            }
        } catch (Throwable e) {
            // Half a copy would only take room: nothing has touched the document yet.
            part.delete();
            removeWithFolder(copy);
            return writeBackFailed(name, e, null);
        }
        android.os.ParcelFileDescriptor descriptor;
        try {
            // From here the document may be emptied: a later start says so if this doesn't finish.
            new File(copy.getParentFile(), DocumentWrite.OPENED).createNewFile();
            // "rwt": the mode Android documents as cut to nothing on opening, as some providers
            // can't cut a file they have handed out; DocumentWrite cuts and checks it again.
            descriptor = getContentResolver().openFileDescriptor(uri, "rwt");
            if (descriptor == null) {
                throw new java.io.IOException(DocumentWrite.UNWRITABLE);
            }
        } catch (Throwable e) {
            // Nothing was written to it: it is as it was, and the copy isn't needed.
            removeWithFolder(copy);
            return writeBackFailed(name, e, null);
        }
        try {
            // Not closed by the stream: a failure closes the descriptor with its error, below.
            FileOutputStream out = new FileOutputStream(descriptor.getFileDescriptor());
            DocumentWrite.overwrite(out.getChannel(), bytes);
            // Closing hands the document back to its provider, which may store it only then (its
            // size as the provider lists it can lag behind: the length was checked on the file).
            descriptor.close();
            descriptor = null;
            removeWithFolder(copy);
            return "project-saved\t" + name;
        } catch (Throwable e) {
            if (descriptor != null) {
                // Its provider is told the write failed, so one that stores on close keeps the
                // document it had.
                try {
                    descriptor.closeWithError(String.valueOf(e.getMessage()));
                } catch (java.io.IOException closing) {
                    // Said below: the write failed.
                }
            }
            // Marked once the Rust side has taken the notice, so the next start doesn't say it again.
            kept[0] = copy;
            return writeBackFailed(name, e, copy);
        }
    }

    /**
     * A write-back-failed event: the document was left as it was ({@code kept} null), or may be
     * damaged, with the complete project kept at {@code kept}. The Rust side words it: the
     * reason is one of DocumentWrite's codes, or the system's own text.
     */
    private static String writeBackFailed(String name, Throwable e, File kept) {
        String reason = e.getMessage() == null ? e.getClass().getSimpleName() : e.getMessage();
        return "write-back-failed\t" + oneField(name) + "\t" + oneField(reason)
                + (kept == null ? "" : "\t" + kept.getAbsolutePath());
    }

    /** Text that fits in one field of an event: no tab or line break. */
    private static String oneField(String text) {
        return text.replaceAll("[\\t\\r\\n]", " ");
    }

    /** Whether this process has looked for write-backs an earlier one didn't finish. */
    private static boolean unfinishedLooked;

    /**
     * Says, once a process, each write-back an earlier process didn't finish (the app ended while
     * it wrote): its complete copy is still in files/saved, unmarked, and the document may be
     * damaged (opened "rwt", it was emptied first). Each is marked once said.
     */
    private void reportUnfinishedWriteBacks() {
        if (unfinishedLooked) {
            return;
        }
        unfinishedLooked = true;
        File[] dirs = new File(getFilesDir(), "saved").listFiles();
        if (dirs == null) {
            return;
        }
        for (File dir : dirs) {
            File copy = DocumentWrite.unfinished(dir);
            if (copy != null) {
                addKept(writeBackFailed(copy.getName(), new java.io.IOException(DocumentWrite.UNFINISHED), copy), copy);
            }
        }
    }

    /** Kept copies whose notice waits to be taken by the Rust side (guarded by events). */
    private final ArrayList<File> toMark = new ArrayList<>();

    /**
     * Adds {@code event}, and with it, under the same lock, the kept copy it names (if any), to be
     * marked as said once takeEvents has handed the event over: never before.
     */
    private void addKept(String event, File copy) {
        synchronized (events) {
            add(event);
            if (copy != null) {
                toMark.add(copy);
            }
        }
    }

    private static void markReported(File copy) {
        File folder = copy.getParentFile();
        if (folder != null) {
            try {
                new File(folder, DocumentWrite.REPORTED).createNewFile();
            } catch (java.io.IOException e) {
                // Said again on a later start at worst.
            }
        }
    }

    /** Removes a file and its folder, once that is empty. */
    private static void removeWithFolder(File file) {
        file.delete();
        File folder = file.getParentFile();
        if (folder != null) {
            // A staging folder's mark goes with it.
            new File(folder, DocumentWrite.OPENED).delete();
            folder.delete();
        }
    }

    /**
     * The document's address when RustGIS may write a project back to it: a project file
     * ({@code .rgis}), granted for writing ({@code grants}), whose provider writes it. The grant
     * lasts for this session: after a restart, Save asks where. Null otherwise.
     */
    private String writable(Uri uri, String name, int grants) {
        // A project file, as the Rust side's is_project_file has it (.rgis, or the older .rgisx).
        String lower = name.toLowerCase(java.util.Locale.ROOT);
        if (!(lower.endsWith(".rgis") || lower.endsWith(".rgisx")) || (grants & Intent.FLAG_GRANT_WRITE_URI_PERMISSION) == 0) {
            return null;
        }
        try (Cursor cursor = getContentResolver().query(uri,
                new String[] {android.provider.DocumentsContract.Document.COLUMN_FLAGS}, null, null, null)) {
            if (cursor == null || !cursor.moveToFirst()
                    || (cursor.getInt(0) & android.provider.DocumentsContract.Document.FLAG_SUPPORTS_WRITE) == 0) {
                return null;
            }
        } catch (RuntimeException e) {
            return null;
        }
        return uri.toString();
    }

    private void saveFailed(boolean project, String name, String reason) {
        add((project ? "project-save-failed\t" : "failed\t") + name + " wasn't saved: " + reason);
    }

    /**
     * What Android allows, in bytes, as "total free": the memory it has (totalMem), and how much
     * of it is free before it counts itself low and ends processes (availMem less threshold).
     * Empty when it can't say. Called from any thread. (getMemoryClass isn't asked: it bounds the
     * Java heap, and the engine's memory is native.)
     */
    public String memory() {
        android.app.ActivityManager.MemoryInfo info = memoryInfo();
        return info == null ? "" : info.totalMem + " " + Math.max(0, info.availMem - info.threshold);
    }

    private android.app.ActivityManager.MemoryInfo memoryInfo() {
        android.app.ActivityManager manager = (android.app.ActivityManager) getSystemService(ACTIVITY_SERVICE);
        if (manager == null) {
            return null;
        }
        android.app.ActivityManager.MemoryInfo info = new android.app.ActivityManager.MemoryInfo();
        manager.getMemoryInfo(info);
        return info;
    }

    /**
     * Android 14 and later tell an app in use nothing when memory runs short (no onLowMemory, no
     * "running" trim level), so it is asked while the app is in sight, at most every
     * MEMORY_LOOK_MILLIS: each time it is found to have turned low is a "low-memory" event.
     */
    private void lookAtMemory() {
        long now = android.os.SystemClock.elapsedRealtime();
        if (!started || now - memoryLooked < MEMORY_LOOK_MILLIS) {
            return;
        }
        memoryLooked = now;
        android.app.ActivityManager.MemoryInfo info = memoryInfo();
        boolean low = info != null && info.lowMemory;
        if (low && !memoryLow) {
            add("low-memory");
        }
        memoryLow = low;
    }

    /**
     * The warnings Android 13 and earlier give a running app, passed on while it is in sight. Out
     * of sight (a track being recorded) it draws no frames to take them, and would drop what it
     * is about to draw once back.
     */
    @Override
    public void onTrimMemory(int level) {
        super.onTrimMemory(level);
        if (started && (level == TRIM_MEMORY_RUNNING_LOW || level == TRIM_MEMORY_RUNNING_CRITICAL)) {
            add("low-memory");
        }
    }

    /** The system as a whole is out of memory (Android 13 and earlier), said while in sight. */
    @Override
    public void onLowMemory() {
        super.onLowMemory();
        if (started) {
            add("low-memory");
        }
    }

    /** What happened since the last call, oldest first, one event to a line. */
    public String takeEvents() {
        lookAtMemory();
        String why = refusal.getAndSet(null);
        if (why != null) {
            keepLocatingRefused();
            add("failed\t" + why);
        }
        synchronized (events) {
            if (typed != null) {
                events.add(typed);
                typed = null;
            }
            String taken = events.take();
            // Every notice waiting has been handed over: the copies they name are said.
            if (events.isEmpty()) {
                for (File copy : toMark) {
                    markReported(copy);
                }
                toMark.clear();
            }
            return taken;
        }
    }

    /** TrackService didn't start: positions stop out of sight again, now if out of sight already. */
    private void keepLocatingRefused() {
        runOnUiThread(() -> {
            keepLocating = false;
            if (!started) {
                stopLocating();
            }
        });
    }

    /**
     * The room the system's bars, a camera cutout and the soft keyboard take at the window's
     * edges, in pixels, as "left top right bottom", then (Android 11 and later) 1 or 0 for
     * whether the keyboard is up; empty before the window is laid out.
     */
    public String insets() {
        return insets;
    }

    private static String describe(android.view.WindowInsets insets) {
        int left, top, right, bottom;
        if (android.os.Build.VERSION.SDK_INT >= 30) {
            android.graphics.Insets bars = insets.getInsets(
                    android.view.WindowInsets.Type.systemBars() | android.view.WindowInsets.Type.displayCutout());
            left = bars.left;
            top = bars.top;
            right = bars.right;
            // The soft keyboard, while it is up, takes the bottom too: what is typed in is kept
            // above it. (The Rust side looks again while a field wants the keyboard, so the
            // room comes back when it goes.)
            bottom = Math.max(bars.bottom, insets.getInsets(android.view.WindowInsets.Type.ime()).bottom);
            // Whether the keyboard is up, which its height alone doesn't tell (it shrinks when
            // the phone turns): the Rust side ends the typing only when it goes.
            boolean keyboard = insets.isVisible(android.view.WindowInsets.Type.ime());
            return left + " " + top + " " + right + " " + bottom + " " + (keyboard ? 1 : 0);
        } else {
            left = insets.getSystemWindowInsetLeft();
            top = insets.getSystemWindowInsetTop();
            right = insets.getSystemWindowInsetRight();
            bottom = insets.getSystemWindowInsetBottom();
        }
        return left + " " + top + " " + right + " " + bottom;
    }

    /** Whether a picker for opening is shown and hasn't answered (its copies may still be running). */
    public boolean picking() {
        return picking;
    }

    /** Whether something asked for is still to be reported. */
    public boolean busy() {
        synchronized (events) {
            return asked > 0 || !events.isEmpty();
        }
    }

    private boolean start(Intent intent, int request) {
        try {
            startActivityForResult(intent, request);
            synchronized (events) {
                asked++;
            }
            return true;
        } catch (RuntimeException e) {
            if (request == SAVE) {
                saveFailed(saveProject, saveName, "the file picker couldn't be shown: " + e.getMessage());
            } else {
                add("failed\tThis device has no file picker: " + e.getMessage());
            }
            return false;
        }
    }

    private void add(String event) {
        synchronized (events) {
            events.add(event);
        }
    }

    /** One thing asked for (a picker shown) is answered in full. */
    private void answered() {
        synchronized (events) {
            asked--;
        }
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request == PHOTO) {
            File file = photo;
            photo = null;
            // Kept only when the camera said it took it and wrote something: a cancelled
            // camera leaves an empty file behind, which goes. One that wrote a photo and then
            // said it was cancelled keeps it, unattached, and that is said.
            if (file != null && result == RESULT_OK && file.length() > 0) {
                add("photo\t" + file.getName() + "\t" + file.getAbsolutePath());
            } else if (file != null && file.length() > 0) {
                add("failed\tThe camera app wrote a photo and then said it was cancelled: it wasn't attached, and is kept as " + file.getAbsolutePath());
            } else {
                if (file != null) {
                    file.delete();
                }
                if (file == null && result == RESULT_OK) {
                    add("failed\tThe camera app took a photo, but RustGIS lost track of it: look in its photos folder.");
                } else {
                    add(result == RESULT_OK ? "failed\tThe camera app said it took a photo, but wrote none." : "cancelled");
                }
            }
            answered();
            return;
        }
        if (request == PICK) {
            picking = false;
            final ArrayList<Uri> uris = new ArrayList<>();
            if (result == RESULT_OK && data != null) {
                ClipData clip = data.getClipData();
                if (clip != null) {
                    for (int i = 0; i < clip.getItemCount(); i++) {
                        uris.add(clip.getItemAt(i).getUri());
                    }
                } else if (data.getData() != null) {
                    uris.add(data.getData());
                }
            }
            if (uris.isEmpty()) {
                add("cancelled");
                answered();
                return;
            }
            final String purpose = this.purpose;
            final int grants = data == null ? 0 : data.getFlags();
            // Copied off the main thread: a document can be large, or come from the network.
            new Thread(() -> {
                // One folder for what was picked together, and for nothing else, with a folder of
                // its own for each folder the documents came from: a file read with the one beside
                // it (a .shp and its .dbf) finds that one, not one of the same name picked from
                // elsewhere, and a later pick can't replace a file the session has open.
                File dir = new File(new File(getFilesDir(), "picked"), Long.toString(System.nanoTime()));
                ArrayList<String> sources = new ArrayList<>();
                // A provider whose ids name no folder for any of the documents picked (the
                // Downloads provider gives a numeric id to a download and a path to other files,
                // both in one folder) keeps all its documents of the pick together, as one folder.
                java.util.HashSet<String> folderless = new java.util.HashSet<>();
                for (Uri uri : uris) {
                    if (sourceFolder(uri) == null) {
                        folderless.add(String.valueOf(uri.getAuthority()));
                    }
                }
                int copies = 0;
                // The pick's folder is marked while documents are copied into it, for the whole
                // pick: Remove Unused Copies keeps it, also between two documents (the name is
                // rustgis_ui_touch::COPYING).
                File mark = new File(dir, ".copying");
                try {
                    if (dir.mkdirs()) {
                        try {
                            try (FileOutputStream active = new FileOutputStream(mark)) {
                                active.write(COPY_OWNER.getBytes(java.nio.charset.StandardCharsets.UTF_8));
                            }
                        } catch (java.io.IOException e) {
                            // Unmarked, the pick is still kept while a copy's .part is in it.
                        }
                    }
                    for (Uri uri : uris) {
                        String folder = sourceFolder(uri);
                        String authority = String.valueOf(uri.getAuthority());
                        String source = authority + "|" + (folder == null || folderless.contains(authority) ? "" : folder);
                        int k = sources.indexOf(source);
                        if (k < 0) {
                            sources.add(source);
                            k = sources.size() - 1;
                        }
                        if (copyIn(purpose, uri, new File(dir, Integer.toString(k)), grants)) {
                            copies++;
                        }
                    }
                } finally {
                    mark.delete();
                    // Nothing copied (each failure said): this request ends, by name, as another
                    // may have been asked for meanwhile (Import DXF stops waiting).
                    if (copies == 0) {
                        // Every copy failed, each removing its own file and folder: this one is
                        // empty too.
                        dir.delete();
                        add("ended\t" + purpose);
                    }
                    answered();
                }
            }).start();
        } else if (request == SAVE) {
            final int grants = data == null ? 0 : data.getFlags();
            final byte[] bytes = saveBytes;
            final String name = saveName;
            final boolean project = saveProject;
            saveBytes = null;
            final Uri uri = result == RESULT_OK && data != null ? data.getData() : null;
            if (uri == null || bytes == null) {
                // Said, as the app has already told the user to choose a place for it.
                saveFailed(project, name, "no place was chosen for it.");
                answered();
                return;
            }
            new Thread(() -> {
                try {
                    try (OutputStream out = getContentResolver().openOutputStream(uri, "wt")) {
                        if (out == null) {
                            throw new java.io.IOException("the place chosen can't be written to");
                        }
                        out.write(bytes);
                    }
                    // Said once the file is closed: a failure to close is a failure to save. A project
                    // saved where RustGIS may write it back names its document first.
                    String document = project ? writable(uri, displayName(uri), grants) : null;
                    if (document != null) {
                        add("project-document\t" + document);
                    }
                    add((project ? "project-saved\t" : "saved\t") + name);
                } catch (Throwable e) {
                    saveFailed(project, name, e.getMessage());
                } finally {
                    answered();
                }
            }).start();
        }
    }

    /**
     * The folder a picked document is in, as far as its provider's id says: the part of the
     * document id before the last "/" (the device's storage gives "primary:Download/roads.shp"),
     * the volume itself for a file at the root of the device's storage ("primary:roads.shp"), or
     * null when the id names no folder (a numeric id, "msf:42").
     */
    private static String sourceFolder(Uri uri) {
        String id;
        try {
            id = android.provider.DocumentsContract.getDocumentId(uri);
        } catch (RuntimeException e) {
            id = uri.getPath() == null ? "" : uri.getPath();
        }
        int slash = id.lastIndexOf('/');
        if (slash >= 0) {
            return id.substring(0, slash);
        }
        int colon = id.indexOf(':');
        if ("com.android.externalstorage.documents".equals(uri.getAuthority()) && colon >= 0) {
            return id.substring(0, colon + 1);
        }
        return null;
    }

    /**
     * Copies a picked document into the app's storage, where the Rust side can open it by path.
     * Returns whether it was copied.
     */
    private boolean copyIn(String purpose, Uri uri, File dir, int grants) {
        String name = displayName(uri);
        File file = new File(dir, name);
        // Two documents of one name picked together (from two folders) both keep their bytes.
        for (int n = 2; file.exists(); n++) {
            file = new File(dir, n + "-" + name);
        }
        // Written under a name of its own, made for this copy alone (".part"), and renamed once
        // whole: Remove Unused Copies keeps a pick holding one, so a copy being written is never
        // taken for unused, and no document of the pick is written over.
        File part = null;
        try {
            if (!dir.isDirectory() && !dir.mkdirs()) {
                throw new java.io.IOException("the app's storage can't be written to");
            }
            part = File.createTempFile(".copy-", ".part", dir);
            try (InputStream in = getContentResolver().openInputStream(uri);
                    OutputStream out = new FileOutputStream(part)) {
                if (in == null) {
                    throw new java.io.IOException("it can't be read");
                }
                byte[] buffer = new byte[COPY_BUFFER];
                for (int n = in.read(buffer); n >= 0; n = in.read(buffer)) {
                    out.write(buffer, 0, n);
                }
            }
            if (!part.renameTo(file)) {
                throw new java.io.IOException("the copy couldn't be given its name");
            }
            // The copy's own name: a second document of one name is told from the first. A project
            // RustGIS may write back to names its document too.
            String document = purpose.equals("project") ? writable(uri, name, grants) : null;
            add("picked\t" + purpose + "\t" + file.getName() + "\t" + file.getAbsolutePath()
                    + (document == null ? "" : "\t" + document));
            return true;
        } catch (Throwable e) {
            // Half a copy would only fill the storage: nothing reports its path to remove it later.
            // (The folder goes only once it is empty.)
            if (part != null) {
                part.delete();
            }
            dir.delete();
            add("failed\t" + name + " wasn't opened: " + e.getMessage());
            return false;
        }
    }

    /** A document's file name, without anything that would put it outside the folder it is copied to. */
    private String displayName(Uri uri) {
        String name = null;
        try (Cursor cursor = getContentResolver().query(uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (cursor != null && cursor.moveToFirst()) {
                name = cursor.getString(0);
            }
        } catch (RuntimeException e) {
            // The name is a convenience: the document is still read below.
        }
        if (name == null || name.isEmpty()) {
            name = "picked";
        }
        return name.replaceAll("[/\\\\\\t\\n\\r\\u0000]", "_").replaceAll("^\\.+", "_");
    }
}
