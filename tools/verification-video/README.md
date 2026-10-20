# OAuth verification video

Google asks apps that request sensitive or restricted scopes for a demo video. Aerie
requests two restricted scopes: `drive.readonly` at sign-in and `drive` on the first
change. Per [Google's demo video guidance](https://support.google.com/cloud/answer/13804565),
the video must show:

- the app as submitted, with its name and branding;
- the complete OAuth consent flow, every one of them (Aerie has two: read-only at sign-in,
  then changes);
- the consent screen with the exact scopes requested, its language set to English;
- the OAuth client ID in the browser's address bar during consent (reviewers check it);
- the app using each scope.

## Record

```bash
tools/verification-video/record.sh
```

It builds a development Aerie and runs it with scratch state and its own keyring entry,
so the installed Aerie and its sign-in are untouched. It records the screen, logs the
focused window, and walks through the steps in `steps.tsv`: do each step on screen, then
press Enter in the terminal. Before starting, open a fresh Chrome window so no other tabs
show, and sign Chrome in to the account reviewers should see. A demo account with a few
sample files keeps real file names out of the video.

## Assemble

```bash
tools/verification-video/assemble.py ~/Videos/aerie-verification/<run> \
    --homepage https://… --privacy-url https://…
```

It frames each step on the window it is about, adds the step's caption, magnifies the
address bar during consent, and adds a title card (app, client ID, scopes and why) and a
closing card (how the data is used). The result is
`<run>/aerie-oauth-verification.mp4`, 1920×1080. Upload it to YouTube as unlisted and give
the link in the verification form.
