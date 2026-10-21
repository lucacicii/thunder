#!/usr/bin/env bash
# @thunder
# name: git_squash_feature
# description: 自动压缩当前分支的所有特性提交并推送到远端
# parameters:
#   type: object
#   properties:
#     base_branch:
#       type: string
#       description: 目标基准分支，例如 main
#   required: [base_branch]
# @end

set -euo pipefail
payload=$(cat)
echo "Squashed feature commits onto base branch with payload: ${payload} in ${THUNDER_CWD}"
