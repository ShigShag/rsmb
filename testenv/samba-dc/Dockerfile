# The Kerberos server: a Samba AD DC (Samba's own Heimdal KDC), provisioned at image build.
FROM alpine:3.24.4
# krb5: MIT kinit, kvno and klist, to make ticket caches inside the container.
RUN apk add --no-cache samba-dc samba-client krb5
# Provisioning writes its own smb.conf; ours replaces it afterwards. xattr_tdb keeps the NT ACLs in
# a file, because a build container may not set security.* extended attributes.
RUN rm -f /etc/samba/smb.conf \
    && { samba-tool domain provision --realm=RSMB.TEST --domain=RSMB --server-role=dc \
            --dns-backend=SAMBA_INTERNAL --host-name=dc --host-ip=10.213.45.12 \
            --adminpass='Adm1n-testpass' \
            --option='vfs objects = dfs_samba4 acl_xattr xattr_tdb' >/tmp/provision.log 2>&1 \
         || { cat /tmp/provision.log; exit 1; }; } \
    && rm /tmp/provision.log
COPY smb.conf /etc/samba/smb.conf
COPY krb5.conf /etc/krb5.conf
COPY mkcache /usr/local/bin/mkcache
RUN chmod +x /usr/local/bin/mkcache \
    && samba-tool domain passwordsettings set --complexity=off --min-pwd-length=1 \
        --min-pwd-age=0 --max-pwd-age=0 --history-length=0 >/dev/null \
    && for u in tester kdirect kproxy kother; do samba-tool user create "$u" testpass >/dev/null; done \
    && for spn in cifs/10.213.45.12 cifs/127.0.0.1 cifs/10.213.45.10; do samba-tool spn add "$spn" 'DC$' >/dev/null; done \
    && mkdir -p /srv/share/sub /srv/share/upload && chmod 1777 /srv/share/upload \
    && echo 'kerberos works' > /srv/share/hello.txt \
    && seq 1 200000 | head -c 1048577 > /srv/share/sub/1m.bin \
    && testparm -s >/dev/null 2>&1
ENTRYPOINT ["samba", "--foreground", "--no-process-group", "--debug-stdout"]
