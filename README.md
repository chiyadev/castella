**NOTE**: As of writing, in 2023, Google has actually started enforcing their storage limit. This project was made public because it is no longer useful. Please do not actually use this project.

# castella

Simple [Google Drive][7]-backed file server, designed to abuse Google's [unenforced storage limit][8] for [Workspace Enterprise Standard][1] users.

## How it works

Instead of implementing the complicated Google OAuth protocol and the Drive API in every application
that seeks to take advantage of Drive, this project aims to provide a simple, highly performant and
self-explanatory HTTP API frontend that proxies requests and responses with automatic authentication,
transparent encryption and rate limit handling.

Various file metadata such as names and encryption keys are stored in a database instead of
Drive itself, in order to keep the amount of data analyzable by Google to the minimum.

Other techniques implemented to circumvent the Drive API limitations are:

- Requests are throttled internally in order to avoid hitting the 20,000 requests/100 second/user rate limit.
- Bandwidth is throttled internally in order to avoid hitting the 750 GB/day upload limit.
- Files are allocated across multiple dynamically created Shared Drives in order to circumvent the 400,000 file limitation.

## Building

This is a [Rust][4] project. Use [Cargo][5] to build the project and deploy the released executable.
Alternatively, the provided [Dockerfile][6] can be used to conveniently build and deploy an image.

Nightly is not required.

## Obtaining the refresh token

An OAuth2 _refresh token_ is used to obtain the _access token_ that is required to access your Drive.

1. Create a project on [Cloud Console][2] under your organization account.
2. Configure an **Internal** OAuth consent screen with the following scopes:

```
https://www.googleapis.com/auth/drive
https://www.googleapis.com/auth/drive.appdata
https://www.googleapis.com/auth/drive.file
https://www.googleapis.com/auth/drive.metadata
```

3. Create a **Web application** OAuth client ID with [OAuth Playground][3] as the authorized redirect URI.
4. Go to [OAuth Playground][3], enter your own OAuth credentials and obtain the refresh token.

## Encryption

The specific encryption algorithm used in castella is a variation of [ChaCha20-Poly1305][9] with a 384-bit key,
64-bit counter and 64-bit nonce.

1. Each file is assigned a unique key and a nonce, both obtained from a CSPRNG.
2. File data is chunked into 4 MiB messages.
3. Each message is encrypted with an incrementing nonce and then authenticated.
4. Messages are concatenated into a single stream and transferred to Drive in the form of a single file.

## License

castella is licensed under the [MIT License](LICENSE), although it is yet to be released publicly at the time of writing.

[1]: https://workspace.google.com/pricing.html
[2]: https://console.cloud.google.com/
[3]: https://developers.google.com/oauthplayground/
[4]: https://www.rust-lang.org/
[5]: https://doc.rust-lang.org/cargo/
[6]: https://docs.docker.com/engine/reference/builder/
[7]: https://drive.google.com/
[8]: https://www.reddit.com/r/DataHoarder/comments/j9rmv3/seems_google_workspace_enterprise_standard_is/
[9]: https://datatracker.ietf.org/doc/html/rfc7539
