  }else {//发行上市
    if (status == "1") {
      return "已受理";
    } else if (status == "2") {
      return "已问询";
    } else if (status == "3") {
      if (subStatus == "1") {
        return "上市委会议<p>通过</p>";
      } else if (subStatus == "2") {
        return "有条件通过";
      } else if (subStatus == "3") {
        return "上市委会议<p>未通过</p>";
      } else if (subStatus == "6") {
        return "暂缓审议";
      }
      return "上市委会议";
    } else if (status == "4") {
      return "提交注册";
    } else if (status == "5") {
      if (registeResult == "1") {
        return "注册生效";
      } else if (registeResult == "2") {
        return "不予注册";
      } else if (registeResult == "3") {
        return "终止注册";
      }
      return "注册结果";
    } else if (status == "6") {
      return "已发行";
    } else if (status == "7") {
      var suspendStatus = data.suspendStatus || '';
      if (suspendStatus == "1") {
        return "中止<p>（财报更新）</p>"
      } else if (suspendStatus == "2") {
        return "中止<p>（其他事项）</p>"
      } else {
        return "中止<p>及财报更新</p>";
      }
    } else if (status == "8") {
      return "终止";
    } else if (status == "9") {
      if (subStatus == "4") {
        return "复审委会议<p>通过</p>";
      } else if (subStatus == "5") {
        return "复审委会议<p>未通过</p>";
      }
      return "复审委会议";
    } else if (status == "10") {
      return "补充审核";
    } else {
      return "-";
    }
  }
}
  

 for (var i = 0; data.result != null && i < data.result.length; i++) {
        var docName = "";
        if (data.result[i].fileType == "30" || data.result[i].fileType == "36" || data.result[i]
          .fileType == "37" || data.result[i].fileType == "32" || data.result[i].fileType ==
          "33") { // 信息披露
          docName = "#tile" + data.result[i].fileType + " .vs" + data.result[i].fileVersion;
        } else if (data.result[i].fileType == "5" || data.result[i].fileType == "6") { // 问询和回复
          var pldate = "";
          pldate += data.result[i].fileUpdateTime.substring(0, 4) + "-" + data.result[i]
            .fileUpdateTime.substring(4, 6) + "-" + data.result[i].fileUpdateTime.substring(
              6, 8);
          docName = "#yjhf tbody";
          appendHtml(docName, "<tr><td style='text-align:center'>" + yjhfCount++ +
            "</td><td><a class='file' target='_blank' href='" + staticFileURI + data
              .result[i].filePath + "'>" + data.result[i].fileTitle +
            "</a></td><td style='text-align:center'>" + pldate + "</td></tr>");
          continue;
        } else if (data.result[i].fileType == "35") { // 注册结果通知
          var pldate = "";
          pldate += data.result[i].fileUpdateTime.substring(0, 4) + "-" + data.result[i]
            .fileUpdateTime.substring(4, 6) + "-" + data.result[i].fileUpdateTime.substring(
              6, 8);
          docName = "#zcjgtz tbody";
          appendHtml(docName, "<tr><td style='text-align:center'>" + zcjgtzCount++ +
            "</td><td><a class='file' target='_blank' href='" + staticFileURI + data
              .result[i].filePath + "'>" + data.result[i].fileTitle +
            "</a></td><td style='text-align:center'>" + pldate + "</td></tr>");
          continue;
        } else if (data.result[i].fileType == "38") { // 终止审核通知
          var pldate = "";
          $(".zzhtz").show();
          pldate += data.result[i].fileUpdateTime.substring(0, 4) + "-" + data.result[i]
            .fileUpdateTime.substring(4, 6) + "-" + data.result[i].fileUpdateTime.substring(
              6, 8);
          docName = "#zzhtz tbody";
          appendHtml(docName, "<tr><td style='text-align:center'>" + zcjgtzCount++ + "</td><td><a class='file' target='_blank' href='" + staticFileURI + data.result[i].filePath + "'>" + data.result[i].fileTitle + "</a></td><td style='text-align:center'>" + pldate + "</td></tr>");
          continue;

        } else {
          docName = "#tile34" + " .vs" + data.result[i].fileVersion;
        }
        var xxpldate = "";
        xxpldate += data.result[i].fileUpdateTime.substring(0, 4) + "-" + data.result[i]
          .fileUpdateTime.substring(4, 6) + "-" + data.result[i].fileUpdateTime.substring(6,
            8);
        appendHtml(docName, "<a class='file' target='_blank' href='" + staticFileURI + data
          .result[i].filePath + "' title='" + data.result[i].fileTitle + "'>" + xxpldate +
          "</a><br>");
      }
    })

  <tr id="tile30">
      <th id="typeDesc">招股说明书</th>
      <td class="vs1">-</td>
      <td class="vs2">-</td>
      <td class="vs3">-</td>
    </tr>
    <tr id="tile36">
      <th>发行保荐书</th>
      <td class="vs1">-</td>
      <td class="vs2">-</td>
      <td class="vs3">-</td>
    </tr>
    <tr id="tile37">
      <th>上市保荐书</th>
      <td class="vs1">-</td>
      <td class="vs2">-</td>
      <td class="vs3">-</td>
    </tr>
    <tr id="tile32">
      <th>审计报告</th>
      <td class="vs1">-</td>
      <td class="vs2">-</td>
      <td class="vs3">-</td>
    </tr>
    <tr id="tile33">
      <th>法律意见书</th>
      <td class="vs1">-</td>
      <td class="vs2">-</td>
      <td class="vs3">-</td>
    </tr>
    <tr id="tile34">
      <th>其他</th>
      <td class="vs1">-</td>
      <td class="vs2">-</td>
      <td class="vs3">-</td>
    </tr>
  </table>