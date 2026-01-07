## Steam Current Game Overlay for OBS

用于显示当前 Steam 正在运行的游戏名称。

![overview](./docs/overview.jpg)


## 配置

环境变量
```
/*
  steam api 地址，默认为官方地址。
  如果需要在大陆使用，请配置为反代地址。
  @rate-limit: 每天 100,000 次请求。
*/
STEAM_API_URL="https://store.steampowered.com"


// 监听地址，默认为本地 3000 端口。
LISTEN_ADDRESS="localhost:3000"
```

### Obs 配置

![obs 配置](./docs/obs-setup.png)
