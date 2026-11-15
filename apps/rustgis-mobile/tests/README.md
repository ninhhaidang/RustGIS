# Phone contract checks

The host-JVM tests exercise position retention, bounded frame batches, completion order and
requested-provider state without an Android device:

```sh
mkdir -p target/phone-contract-tests
javac -d target/phone-contract-tests \
  apps/rustgis-mobile/java/org/rustgis/mobile/EventBuffer.java \
  apps/rustgis-mobile/java/org/rustgis/mobile/LocationProviders.java \
  apps/rustgis-mobile/tests/java/org/rustgis/mobile/PhoneContractsTest.java
java -ea -cp target/phone-contract-tests org.rustgis.mobile.PhoneContractsTest
```

The shared Rust shell's fake-phone tests cover two picker requests in one frame and injected
worker admission failure. They run on the host through `cargo test -p rustgis-mobile shell::tests`.
The Android activity is also compiled by `build-apk.sh`; `cargo check -p rustgis-mobile --target
aarch64-linux-android` checks the Rust shell against its Android APIs with a configured NDK.
