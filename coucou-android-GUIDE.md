# Coucou for Android

The Android companion to Coucou (Windows/Mac). It shows your AI coding-agent
sessions and lets you **Allow / Deny approval requests from your phone** — the
cross-platform twin of the iPhone app (which uses Apple-only CloudKit; Android
pairs through a Firebase project you own).

This first version covers **sessions + approvals**. Questions, instructions,
services and widgets can come later.

## How it works

```
 Coucou (Windows)                Firebase                     Android phone
 ────────────────                ────────                     ─────────────
 publishes sessions  ──PATCH──►  Firestore  ──realtime────►   sessions list
 + approval docs                 pairs/{id}                   approval card
                                    │
 new approval doc ──────────────►  └─► Cloud Function ──FCM──► push + Allow/Deny
 reads decisions   ◄──poll──────  decisions  ◄──write────────  you tap Allow/Deny
 (applies them)
```

Everything is scoped under `pairs/{pairId}`; the pair id (shown on the PC) is the
shared secret. Only your own Firebase project is involved — no third-party server.

## Build (Android Studio)

1. **Create a Firebase project** (free) at <https://console.firebase.google.com>.
2. In it: **Build → Firestore Database → Create** (production mode). **Authentication → Sign-in method → Anonymous → Enable**. Cloud Messaging is on by default.
3. **Add an Android app** with package name `fr.louisraille.coucou`. Download the
   generated **`google-services.json`** and drop it into `android/app/`.
4. Open the `android/` folder in **Android Studio** (Giraffe or newer). Let it sync.
5. Run on your phone (USB debugging) or **Build → Build Bundle(s)/APK(s) → Build APK**.

## Deploy the rules + push function

From the `firebase/` folder (needs the Firebase CLI: `npm i -g firebase-tools`, then `firebase login`):

```
cd firebase
firebase use <your-project-id>
firebase deploy --only firestore:rules,functions
```

(The push function needs the Firebase **Blaze** plan — still free within the
generous monthly quota. Without it, approvals still appear live while the app is
open; you only lose background push.)

## Pair

1. On the PC: **Coucou → Settings → Phone (Android companion)** → turn **Mirror to
   phone** on, paste your Firebase **Web API key** and **project id**, and copy the
   **pair code**.
2. In the Android app: enter the pair code, tap **Link**.

That's it — sessions appear, and approval requests show with **Allow / Deny**, on
screen and as a push notification.
