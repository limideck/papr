
在本机（已配好 scripts/deploy.env）：

scripts/restart.sh
# 或
scripts/deploy.sh --restart-only


或在服务器上

systemctl restart papr-server
systemctl status papr-server
curl -s http://127.0.0.1:7400/api/health



git tag v0.0.1
git push origin v0.0.1



scripts/deploy.sh --from-release latest
