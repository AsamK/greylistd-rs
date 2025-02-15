# greylistd-rs

greylistd-rs is a reimplementation of the [greylistd](https://salsa.debian.org/debian/greylistd/) daemon for use with Exim 4.
It is a drop-in replacement for the greylistd python daemon.

For documentation see the original man pages:
- [greylist(1)](https://manpages.debian.org/stable/greylistd/greylist.1.html)
- [greylistd(8)](https://manpages.debian.org/stable/greylistd/greylistd.8.html)

greylistd-rs was written due to two longstanding bugs in the original greylistd ([unstable hashing](https://bugs.debian.org/cgi-bin/bugreport.cgi?bug=1021356) and failure to save at exit with systemd socket).
It supports one new data option `onlysubnet=true`, that when enabled doesn't match the whole IP address, but only the subnet (/24 for IPv4 and /64 for IPv6).

## Building

This project uses cargo (MSRV 1.81.0) for building and maintaining dependencies.

1. Checkout the source somewhere on your filesystem with

       git clone https://github.com/AsamK/greylistd-rs.git

2. Execute cargo:

       cargo build

## Installation

greylistd-rs currently only implements the daemon part of greylistd so the original package should be installed and adapted via a systemd override config file.

```sh
sudo apt install greylistd
sudo cp target/{debug,release}/greylistd /usr/sbin/greylistd-rs
echo "[Service]
ExecStart=
ExecStart=/usr/sbin/greylistd-rs" | sudo tee /etc/systemd/system/greylistd.service.d/override.conf
sudo systemctl daemon-reload
sudo systemctl restart greylistd
```

## License

Licensed under the GPLv3: http://www.gnu.org/licenses/gpl-3.0.html
