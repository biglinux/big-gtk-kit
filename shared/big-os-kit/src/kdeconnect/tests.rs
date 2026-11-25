#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "the fixture starts a private bus daemon"
)]

use super::*;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use zbus::connection::Builder;
use zbus::zvariant::Value;

const RUNTIME: &str = "/run/user/1000";

fn parse(text: &str) -> Option<Location> {
    Location::parse(text, Path::new(RUNTIME))
}

fn location(device: &str, rest: &str) -> Option<Location> {
    Some(Location {
        device: device.to_owned(),
        rest: PathBuf::from(rest),
    })
}

#[test]
fn uri_names_device_and_folder() {
    assert_eq!(
        parse("kdeconnect://abc_1/DCIM/Camera%20Roll"),
        location("abc_1", "DCIM/Camera Roll")
    );
    assert_eq!(parse("KDEConnect://abc_1"), location("abc_1", ""));
    assert_eq!(parse("kdeconnect://abc_1/"), location("abc_1", ""));
}

#[test]
fn fuse_path_names_device_folder() {
    assert_eq!(
        parse("/run/user/1000/kio-fuse-Ab3xYz/kdeconnect/abc_1/DCIM/Camera"),
        location("abc_1", "DCIM/Camera")
    );
    assert_eq!(
        parse("/run/user/1000/kio-fuse-Ab3xYz/kdeconnect/abc_1"),
        location("abc_1", "")
    );
}

#[test]
fn foreign_or_hostile_places_are_refused() {
    for text in [
        "sftp://abc_1/DCIM",
        "/home/me/kdeconnect/abc_1",
        "/run/user/1001/kio-fuse-Ab3xYz/kdeconnect/abc_1",
        "/run/user/1000/kio-fuse-Ab3xYz/smb/abc_1",
        "/run/user/1000/other/kdeconnect/abc_1",
        "kdeconnect:///DCIM",
        "kdeconnect://abc_1/../../etc",
        "kdeconnect://abc_1/a%2F..%2Fb/%2e%2e",
        "kdeconnect://abc_1/a%00b",
        "kdeconnect://abc_1/%zz",
        "kdeconnect://abc_1/DCIM?x",
        "kdeconnect://abc_1/DCIM#top",
        "kdeconnect://abc%2F1/x",
        "kdeconnect://a-b/x",
        "/run/user/1000/kio-fuse-Ab3xYz/kdeconnect/abc_1/../x",
    ] {
        assert_eq!(parse(text), None, "{text}");
    }
}

fn storage(path: &str, name: &str) -> Storage {
    Storage {
        path: PathBuf::from(path),
        name: name.to_owned(),
    }
}

#[test]
fn storage_name_selects_its_storage() {
    let storages = [
        storage("/m/storage/1234-ABCD", "SD card"),
        storage("/m/storage/emulated/0", "Internal storage"),
    ];
    assert_eq!(
        place(&storages, Path::new("SD card/Music")),
        Some(PathBuf::from("/m/storage/1234-ABCD/Music"))
    );
    assert_eq!(
        place(&storages, Path::new("DCIM/Camera")),
        Some(PathBuf::from("/m/storage/emulated/0/DCIM/Camera"))
    );
    assert_eq!(
        place(&storages, Path::new("")),
        Some(PathBuf::from("/m/storage/emulated/0"))
    );
    assert_eq!(place(&[], Path::new("DCIM")), None);
}

// A private bus that serves no activation files: a call to a name nobody owns
// must fail, never start the real daemon.
struct Bus {
    child: Child,
    address: String,
    config: PathBuf,
}

impl Bus {
    fn start() -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let config = std::env::temp_dir().join(format!(
            "kdeconnect-bus-{}-{}.conf",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::write(
            &config,
            r#"<busconfig><type>session</type><listen>unix:tmpdir=/dev/shm</listen><auth>EXTERNAL</auth>
<policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>"#,
        )
        .expect("bus config");
        let mut child = Command::new("dbus-daemon")
            .arg("--config-file")
            .arg(&config)
            .args(["--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon");
        let mut address = String::new();
        BufReader::new(child.stdout.take().expect("stdout"))
            .read_line(&mut address)
            .expect("bus address");
        Self {
            child,
            address: address.trim().to_owned(),
            config,
        }
    }

    async fn connect(&self) -> Connection {
        Builder::address(self.address.as_str())
            .expect("address")
            .build()
            .await
            .expect("connection")
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.config);
    }
}

struct FakeDaemon(Vec<(&'static str, bool, bool)>);

#[zbus::interface(name = "org.kde.kdeconnect.daemon")]
impl FakeDaemon {
    #[zbus(name = "devices")]
    fn devices(&self, only_reachable: bool, only_paired: bool) -> Vec<String> {
        self.0
            .iter()
            .filter(|(_, reachable, paired)| {
                (*reachable || !only_reachable) && (*paired || !only_paired)
            })
            .map(|(id, ..)| (*id).to_owned())
            .collect()
    }

    #[zbus(signal, name = "deviceListChanged")]
    async fn device_list_changed(
        emitter: &zbus::object_server::SignalEmitter<'_>,
    ) -> zbus::Result<()>;
}

struct FakeDevice {
    name: &'static str,
    plugins: &'static [&'static str],
    reachable: bool,
    paired: bool,
}

#[zbus::interface(name = "org.kde.kdeconnect.device")]
impl FakeDevice {
    #[zbus(property, name = "name")]
    fn name(&self) -> String {
        self.name.to_owned()
    }

    #[zbus(property, name = "isReachable")]
    fn is_reachable(&self) -> bool {
        self.reachable
    }

    #[zbus(property, name = "isPaired")]
    fn is_paired(&self) -> bool {
        self.paired
    }

    #[zbus(name = "hasPlugin")]
    fn has_plugin(&self, plugin: &str) -> bool {
        self.plugins.contains(&plugin)
    }
}

struct FakeShare(Arc<Mutex<Vec<String>>>);

#[zbus::interface(name = "org.kde.kdeconnect.device.share")]
impl FakeShare {
    #[zbus(name = "shareUrls")]
    fn share_urls(&self, urls: Vec<String>) {
        self.0.lock().expect("urls").extend(urls);
    }
}

/// Records a send as the entry `clipboard` in the list the share object fills.
struct FakeClipboard(Arc<Mutex<Vec<String>>>);

#[zbus::interface(name = "org.kde.kdeconnect.device.clipboard")]
impl FakeClipboard {
    #[zbus(name = "sendClipboard")]
    fn send_clipboard(&self) {
        self.0.lock().expect("calls").push("clipboard".to_owned());
    }
}

struct FakeSftp {
    mounted: bool,
    error: &'static str,
}

#[zbus::interface(name = "org.kde.kdeconnect.device.sftp")]
impl FakeSftp {
    #[zbus(name = "mountAndWait")]
    fn mount_and_wait(&self) -> bool {
        self.mounted
    }

    #[zbus(name = "getMountError")]
    fn mount_error(&self) -> String {
        self.error.to_owned()
    }

    #[zbus(name = "mountPoint")]
    fn mount_point(&self) -> String {
        "/run/user/1000/phone".to_owned()
    }

    #[zbus(name = "getDirectories")]
    fn directories(&self) -> HashMap<String, Value<'static>> {
        HashMap::from([
            (
                "/run/user/1000/phone/storage/emulated/0".to_owned(),
                Value::from("Internal storage"),
            ),
            (
                "/run/user/1000/phone/../etc".to_owned(),
                Value::from("Climbs out"),
            ),
            ("/etc".to_owned(), Value::from("Elsewhere")),
        ])
    }
}

/// A daemon with four phones: `a` takes files, `b` has no share plugin, `c` is
/// out of reach, `d` is not paired. Returns the connection that must stay alive and the urls `a` got.
async fn serve(bus: &Bus, mounted: bool) -> (Connection, Arc<Mutex<Vec<String>>>) {
    let urls = Arc::default();
    let builder = Builder::address(bus.address.as_str())
        .expect("address")
        .name(SERVICE)
        .expect("name")
        .serve_at(
            DAEMON_PATH,
            FakeDaemon(vec![
                ("a", true, true),
                ("b", true, true),
                ("c", false, true),
                ("d", true, false),
            ]),
        )
        .expect("daemon")
        .serve_at(
            "/modules/kdeconnect/devices/a",
            FakeDevice {
                name: "Alice",
                plugins: &[SHARE_PLUGIN, SFTP_PLUGIN],
                reachable: true,
                paired: true,
            },
        )
        .expect("a")
        .serve_at(
            "/modules/kdeconnect/devices/b",
            FakeDevice {
                name: "Bob",
                plugins: &[SFTP_PLUGIN],
                reachable: true,
                paired: true,
            },
        )
        .expect("b")
        .serve_at(
            "/modules/kdeconnect/devices/c",
            FakeDevice {
                name: "Cy",
                plugins: &[SHARE_PLUGIN],
                reachable: false,
                paired: true,
            },
        )
        .expect("c")
        .serve_at(
            "/modules/kdeconnect/devices/d",
            FakeDevice {
                name: "Di",
                plugins: &[],
                reachable: true,
                paired: false,
            },
        )
        .expect("d")
        .serve_at(
            "/modules/kdeconnect/devices/a/share",
            FakeShare(Arc::clone(&urls)),
        )
        .expect("share")
        .serve_at(
            "/modules/kdeconnect/devices/a/clipboard",
            FakeClipboard(Arc::clone(&urls)),
        )
        .expect("clipboard")
        .serve_at(
            "/modules/kdeconnect/devices/a/sftp",
            FakeSftp {
                mounted,
                error: "sshfs exited with status 1",
            },
        )
        .expect("sftp");
    (builder.build().await.expect("service"), urls)
}

#[test]
fn only_reachable_sharing_phones_are_listed() {
    let bus = Bus::start();
    zbus::block_on(async {
        let (_service, _) = serve(&bus, true).await;
        let phones = phones_with(&bus.connect().await, SHARE_PLUGIN)
            .await
            .expect("phones");
        assert_eq!(
            phones,
            [Phone {
                id: "a".into(),
                name: "Alice".into()
            }]
        );
    });
}

#[test]
fn storage_phones_follow_their_plugin() {
    let bus = Bus::start();
    zbus::block_on(async {
        let (_service, _) = serve(&bus, true).await;
        let names: Vec<String> = phones_with(&bus.connect().await, SFTP_PLUGIN)
            .await
            .expect("phones")
            .into_iter()
            .map(|phone| phone.name)
            .collect();
        assert_eq!(names, ["Alice", "Bob"]);
    });
}

#[test]
fn unavailable_device_says_why() {
    let bus = Bus::start();
    zbus::block_on(async {
        let (_service, _) = serve(&bus, true).await;
        let client = bus.connect().await;
        let why = |id, plugin| unavailable(&client, id, plugin);
        assert_eq!(why("z", SHARE_PLUGIN).await, Some(Unavailable::Unknown));
        assert_eq!(why("d", SHARE_PLUGIN).await, Some(Unavailable::NotPaired));
        assert_eq!(
            why("c", SHARE_PLUGIN).await,
            Some(Unavailable::NotReachable)
        );
        assert_eq!(why("b", SHARE_PLUGIN).await, Some(Unavailable::PluginOff));
        assert_eq!(why("a", SHARE_PLUGIN).await, None);
    });
    let bus = Bus::start();
    zbus::block_on(async {
        let client = bus.connect().await;
        assert_eq!(
            unavailable(&client, "a", SHARE_PLUGIN).await,
            Some(Unavailable::NotRunning)
        );
    });
}

#[test]
fn stopped_daemon_is_never_called() {
    let bus = Bus::start();
    zbus::block_on(async {
        let client = bus.connect().await;
        assert_eq!(
            phones_with(&client, SHARE_PLUGIN).await.expect("no error"),
            []
        );
        assert!(matches!(
            mount(&client, "a").await,
            Err(MountError::NotRunning)
        ));
    });
}

#[test]
fn shared_urls_reach_the_phone() {
    let bus = Bus::start();
    zbus::block_on(async {
        let (_service, urls) = serve(&bus, true).await;
        let sent = [
            "file:///home/me/a%20b.txt".to_owned(),
            "file:///home/me/c.txt".to_owned(),
        ];
        share_urls(&bus.connect().await, "a", &sent)
            .await
            .expect("share");
        assert_eq!(*urls.lock().expect("urls"), sent);
    });
}

#[test]
fn clipboard_send_reaches_the_phone() {
    let bus = Bus::start();
    zbus::block_on(async {
        let (_service, calls) = serve(&bus, true).await;
        let client = bus.connect().await;
        send_clipboard(&client, "a").await.expect("sent");
        assert_eq!(*calls.lock().expect("calls"), ["clipboard"]);
        assert!(send_clipboard(&client, "b").await.is_err());
    });
}

#[test]
fn mount_lists_storage_paths() {
    let bus = Bus::start();
    zbus::block_on(async {
        let (_service, _) = serve(&bus, true).await;
        let storages = mount(&bus.connect().await, "a").await.expect("mounted");
        assert_eq!(
            storages,
            [storage(
                "/run/user/1000/phone/storage/emulated/0",
                "Internal storage"
            )]
        );
    });
}

#[test]
fn refused_mount_carries_the_reason() {
    let bus = Bus::start();
    zbus::block_on(async {
        let (_service, _) = serve(&bus, false).await;
        let Err(MountError::Refused(reason)) = mount(&bus.connect().await, "a").await else {
            panic!("the mount should be refused");
        };
        assert_eq!(reason, "sshfs exited with status 1");
    });
}

#[test]
fn device_list_signal_ticks_the_watch() {
    let bus = Bus::start();
    zbus::block_on(async {
        let (service, _) = serve(&bus, true).await;
        let client = bus.connect().await;
        let mut ticks = Box::pin(phones_changed(&client).await.expect("watch"));
        let emitter =
            zbus::object_server::SignalEmitter::new(&service, DAEMON_PATH).expect("emitter");
        FakeDaemon::device_list_changed(&emitter)
            .await
            .expect("emit");
        assert!(ticks.next().await.is_some());
    });
}

async fn yield_once() {
    let mut yielded = false;
    std::future::poll_fn(|cx| {
        if std::mem::replace(&mut yielded, true) {
            return std::task::Poll::Ready(());
        }
        cx.waker().wake_by_ref();
        std::task::Poll::Pending
    })
    .await;
}

#[test]
fn followed_list_updates_on_each_change() {
    let bus = Bus::start();
    zbus::block_on(async {
        let (service, _) = serve(&bus, true).await;
        let client = bus.connect().await;
        let emitter =
            zbus::object_server::SignalEmitter::new(&service, DAEMON_PATH).expect("emitter");
        let first_read = std::sync::atomic::AtomicBool::new(false);
        let mut seen = 0;
        let follow = follow_phones(&client, SHARE_PLUGIN, |phones| {
            assert_eq!(phones.len(), 1);
            seen += 1;
            first_read.store(true, std::sync::atomic::Ordering::SeqCst);
            seen < 2
        });
        // The change is signalled once the follower has subscribed and read.
        let change = async {
            while !first_read.load(std::sync::atomic::Ordering::SeqCst) {
                yield_once().await;
            }
            FakeDaemon::device_list_changed(&emitter)
                .await
                .expect("emit");
            std::future::pending::<()>().await;
        };
        futures_util::future::select(Box::pin(follow), Box::pin(change)).await;
        assert_eq!(seen, 2);
    });
}
