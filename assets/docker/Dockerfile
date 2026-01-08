# 使用极其精简的 Alpine Linux 镜像 (适合 musl 静态编译)
# 如果你是用 gnu 编译的，请将下面这行换成: FROM debian:bullseye-slim
FROM alpine:latest

# 1. 安装基础依赖
# ca-certificates: 必须！用于 HTTPS 请求 (访问 Steam API)
# tzdata: 可选，用于设置正确的时区
# gcompat: 可选，如果你不是静态编译的，这个库能让 Alpine 运行部分 glibc 程序
RUN apk add --no-cache ca-certificates tzdata gcompat

# 2. 设置工作目录
WORKDIR /app

# 3. 复制二进制文件到容器内
COPY SteamCurrentGameServer .

# 4. 赋予执行权限 (防止上传时权限丢失)
RUN chmod +x SteamCurrentGameServer

# 5. 暴露端口 (仅做文档说明)
EXPOSE 3000

# 6. 启动命令
CMD ["./SteamCurrentGameServer"]
