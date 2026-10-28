#!/bin/sh
# Rodeo 部署包启动脚本：后台常驻。
#
# 用法：
#   ./start.sh              启动（立刻返回，rodeo 在后台跑）
#   kill $(cat rodeo.pid)   停掉
#   tail -f rodeo.log       看日志
#
# 环境变量：
#   LEPTOS_SITE_ROOT=site   站点静态资源目录（包内 site/），Leptos 运行期读它拼资源路径。
#   LEPTOS_HASH_FILES=true  与包内 hash.txt 配套：打开后 site/pkg 里是 rodeo.<hash>.js/wasm/css，
#                           HTML 才会带上同样的哈希；否则它按裸名 /pkg/rodeo.js 去请求，
#                           那些文件并不存在，前端资源一律 404。两者缺一不可。
#   刻意不设 LEPTOS_SITE_ADDR：它的优先级高于 config.toml 的 [server]，在这里导出等于
#                           把生产机上的配置悄悄盖掉，正是要避免的那种静默失效。
set -e
cd "$(dirname "$0")"

export LEPTOS_SITE_ROOT=site
export LEPTOS_HASH_FILES=true

# 二进制的可执行位可能在传输途中丢掉：zip 中转、不保留模式的挂载、跨文件系统拷贝都会。
# 结果是包里有 rodeo 却跑不起来，报的是 "Permission denied"。这里补一次，省得人工 chmod。
chmod +x ./rodeo

# 包里只带 config.example.toml，没带 config.toml。缺了会退回内置默认配置（端口、数据目录
# 都是默认值），后台启动又不像前台那样一眼看得见，只提示不拦。
if [ ! -f config.toml ]; then
    echo "提示：包内没有 config.toml，先 cp config.example.toml config.toml 再按需修改。" >&2
fi

# 后台常驻：nohup 屏蔽 SIGHUP（终端断开后进程不被连带杀掉），日志追加到 rodeo.log。
nohup ./rodeo config.toml >> rodeo.log 2>&1 &
pid=$!
echo "$pid" > rodeo.pid

# 起一秒后确认进程还活着。配置写错、端口被占、存储打不开这类问题都写进日志里了，
# 脚本直接把日志尾巴打出来，免得人再去翻——后台启动最容易出的岔子就是"命令返回了，
# 其实没起来"。
sleep 1
if ! kill -0 "$pid" 2>/dev/null; then
    echo "rodeo 启动失败，日志末尾如下：" >&2
    tail -n 30 rodeo.log >&2 || true
    exit 1
fi

# 花括号不能省：变量名后面紧跟中文标点时，bash（不少发行版的 /bin/sh 就是它）会把
# 多字节字符当成变量名的一部分，$pid， 会被解析成一个不存在的变量，pid 就印不出来了。
echo "rodeo 已在后台启动：pid=${pid}，日志=$(pwd)/rodeo.log"
