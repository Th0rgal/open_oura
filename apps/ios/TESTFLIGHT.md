# Shipping OuraApp to TestFlight

The simulator dev harness (`OuraApp/build_run.sh`) is for quick local runs. TestFlight
needs a **signed device archive**. Everything below the signing step is scaffolded;
signing requires *your* Apple Developer account.

## One-time
- **Apple Developer Program** membership ($99/yr).
- Register the App ID **`com.openoura.app`** and create the app in App Store Connect.
- Install xcodegen: `brew install xcodegen`.

## Build & upload
```bash
# 1. shared Rust core → both device + simulator slices
./apps/ios/build-xcframework.sh

# 2. generate the Xcode project from project.yml
cd apps/ios/OuraApp && xcodegen generate

# 3. open it, set your Team under Signing & Capabilities (or DEVELOPMENT_TEAM in project.yml)
open OuraApp.xcodeproj
#    then: Product → Archive → Distribute App → TestFlight & App Store
```
Or headless once a Team is set:
```bash
xcodebuild -project OuraApp.xcodeproj -scheme OuraApp -sdk iphoneos \
  -configuration Release archive -archivePath build/OuraApp.xcarchive
xcodebuild -exportArchive -archivePath build/OuraApp.xcarchive \
  -exportOptionsPlist ExportOptions.plist -exportPath build/export   # then upload with `xcrun altool`/Transporter
```

## Already handled
- App icon (`Assets.xcassets/AppIcon.appiconset`, 1024²).
- `Info.plist`: Bluetooth usage strings; `ITSAppUsesNonExemptEncryption=false` (AES ring
  auth is exempt); simulator platform pin removed so a device archive is valid.
- Device (`ios-arm64`) **and** simulator slices in `OuraCore.xcframework`.
- Both sim and device Release builds verified to compile + link.

## Still on you
- **Signing**: Team ID + a distribution provisioning profile (only you can do this).
- **Version bumps**: `MARKETING_VERSION` / `CURRENT_PROJECT_VERSION` in `project.yml`.
- **Data**: the build does **not** bundle `oura.db` (it's your personal health data). A
  real beta should sync each tester's own ring over BLE; for a personal-only build, drop
  an `oura.db` into the bundle (see the note in `project.yml`). Shipping your own DB to
  testers would expose your health data.
