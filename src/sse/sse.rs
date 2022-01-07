use anyhow::{anyhow, Context, Error};
use reqwest::{header, Client, ClientBuilder, Url};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use time::macros::date;
use time::{format_description, Date, PrimitiveDateTime};
use tokio::fs::File;
use tokio::io::copy;
use tokio::sync::Mutex;

static DECLARE_SUBFOLDER: &str = "申报稿";
static REGISTER_SUBFOLDER: &str = "注册稿";
static MEETING_SUBFOLDER: &str = "上会稿";
static SPONSOR_SUBFOLDER: &str = "问询与回复/发行人与保荐机构";
static ACCOUNTANT_SUBFOLDER: &str = "问询与回复/会计师";
static LAWYER_SUBFOLDER: &str = "问询与回复/律师";
static RESULT_SUBFOLDER: &str = "结果";

static SUBFOLDERS: [&str; 7] = [
    DECLARE_SUBFOLDER,
    REGISTER_SUBFOLDER,
    MEETING_SUBFOLDER,
    SPONSOR_SUBFOLDER,
    ACCOUNTANT_SUBFOLDER,
    LAWYER_SUBFOLDER,
    RESULT_SUBFOLDER,
];

/// IPO result
#[repr(u8)]
#[derive(Debug)]
pub enum RegisterResult {
    // 1 - 注册生效
    RegisterEffective(Date),
    // 3 - 终止注册
    RegisterTerminated(Date),
}

/// audit status of IPO
#[repr(u8)]
#[derive(Debug)]
pub enum AuditStatus {
    // 1 - 已受理
    Accepted(Date),
    // 2 - 已问询
    Queried(Date),
    // 3 - 上市委会议
    Discussed(Date),
    // 4 - 提交注册
    Submitted(Date),
    // 5 - 注册生效 or 终止注册
    Registered(RegisterResult),
    // other
    // Todo
    Unsupported,
}

/// the information about company which want to IPO in KCB
#[derive(Debug)]
pub struct CompanyInfo {
    // the company name
    stock_audit_name: String,
    // the audit id assigned to the company by see
    stock_audit_number: u32,
    // current status
    curr_status: AuditStatus,
    // the date submitting application
    apply_date: PrimitiveDateTime,
    // the date update information
    update_date: PrimitiveDateTime,
}

fn parse_date_from_sse(input: &str) -> anyhow::Result<PrimitiveDateTime> {
    let format = format_description::parse("[year][month][day][hour][minute][second]")?;
    let ret = PrimitiveDateTime::parse(input, &format)?;
    Ok(ret)
}

impl TryFrom<String> for CompanyInfo {
    type Error = anyhow::Error;
    fn try_from(resp: String) -> Result<Self, Self::Error> {
        #[allow(clippy::useless_format)]
        let json_str = format!(
            r#"{}"#,
            resp.split_terminator(&['(', ')'][..])
                .next_back()
                .context("invalid input")?
        );
        let json_body: Value = serde_json::from_str(&json_str)?;
        if matches!(&json_body["result"], Value::Array(result) if result.is_empty()) {
            return Err(anyhow!("empty company info"));
        }
        Ok(CompanyInfo {
            stock_audit_name: {
                let company_name = json_body["result"][0]["stockAuditName"].as_str();
                if let Some(temp) = company_name {
                    temp.to_owned()
                } else {
                    return Err(anyhow!("get company name failed"));
                }
            },
            stock_audit_number: {
                let number = json_body["result"][0]["stockAuditNum"].as_str().unwrap();
                number.parse::<u32>().unwrap()
            },
            curr_status: {
                let status = json_body["result"][0]["currStatus"].as_u64();
                let result = json_body["result"][0]["registeResult"].as_u64();
                let update_date = json_body["result"][0]["updateDate"]
                    .as_str()
                    .context("acquire update time failed")?;
                let date = parse_date_from_sse(update_date)?;
                match (status, result) {
                    (Some(5), Some(1)) => {
                        AuditStatus::Registered(RegisterResult::RegisterEffective(date.date()))
                    }
                    (Some(5), Some(3)) => {
                        AuditStatus::Registered(RegisterResult::RegisterTerminated(date.date()))
                    }
                    (Some(4), _) => AuditStatus::Submitted(date.date()),
                    (Some(3), _) => AuditStatus::Discussed(date.date()),
                    (Some(2), _) => AuditStatus::Queried(date.date()),
                    (Some(1), _) => AuditStatus::Accepted(date.date()),
                    (_, _) => AuditStatus::Unsupported,
                }
            },
            apply_date: {
                let apply_date = json_body["result"][0]["auditApplyDate"]
                    .as_str()
                    .context("acquire apply_date failed")?;
                parse_date_from_sse(apply_date)?
            },
            update_date: {
                let update_date = json_body["result"][0]["updateDate"]
                    .as_str()
                    .context("acquire update_date failed")?;
                parse_date_from_sse(update_date)?
            },
        })
    }
}

#[derive(Debug)]
pub struct UploadFile {
    filename: String,
    url: Url,
    path: PathBuf,
}

#[derive(Debug)]
pub enum QueryReply {
    // 发行人,保荐机构
    Sponsor(UploadFile),
    // 会计师
    Accountant(UploadFile),
    // 律师
    Lawyer(UploadFile),
    // other
    Other(UploadFile),
}

/// 信息披露 & 问询与回复 & 注册结果文件
#[derive(Debug, Default)]
pub struct InfoDisclosure {
    /* #### 信息披露
     * ----
     * +. 1st element: 申报稿
     * +. 2nd element: 上会稿
     * +. 3rd element: 注册稿
     *
     */
    // 招股说明书
    prospectuses: [Option<UploadFile>; 3],
    // 发行保荐书
    publish_sponsor: [Option<UploadFile>; 3],
    // 上市保荐书
    list_sponsor: [Option<UploadFile>; 3],
    // 审计报告
    audit_report: [Option<UploadFile>; 3],
    // 法律意见书
    legal_opinion: [Option<UploadFile>; 3],
    // 其他
    others: [Option<UploadFile>; 3],
    /* #### 问询与回复
     * ----
     */
    query_and_reply: Vec<Option<QueryReply>>,
    /* #### 注册结果文件 and 终止审核通知
     * ----
     */
    register_result_or_audit_terminated: Vec<Option<UploadFile>>,
}

impl TryFrom<String> for InfoDisclosure {
    type Error = anyhow::Error;
    fn try_from(resp: String) -> Result<Self, Self::Error> {
        #[allow(clippy::useless_format)]
        let json_str = format!(
            r#"{}"#,
            resp.split_terminator(&['(', ')'][..])
                .next_back()
                .context("invalid input")?
        );
        let json_body: Value = serde_json::from_str(&json_str)?;
        let mut infos = InfoDisclosure::default();
        let file_arr = json_body["result"]
            .as_array()
            .context("extract file array failed")?;
        let download_base = Url::parse("http://static.sse.com.cn/stock")?;
        let ret = file_arr.iter().try_for_each(|x| {
            let mut file = UploadFile {
                filename: {
                    let name = x["fileTitle"].as_str().context("get filename failed")?;
                    name.to_owned()
                },
                url: {
                    let download_url = x["filePath"].as_str().context("get file url failed")?;
                    download_base.join(download_url)?
                },
                path: {
                    let mut path = PathBuf::new();
                    path.push("Download");
                    path.push(
                        x["companyFullName"]
                            .as_str()
                            .context("get company name failed")?,
                    );
                    path
                },
            };
            let file_type = x["fileType"].as_u64();
            let file_ver = x["fileVersion"].as_u64();
            match (file_type, file_ver) {
                // 招股说明书, 申报稿
                (Some(30), Some(1)) => {
                    file.path.push(DECLARE_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.prospectuses[0] = Some(file);
                    Ok(())
                }
                // 招股说明书, 上会稿
                (Some(30), Some(2)) => {
                    file.path.push(MEETING_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.prospectuses[1] = Some(file);
                    Ok(())
                }
                // 招股说明书, 注册稿
                (Some(30), Some(3)) => {
                    file.path.push(REGISTER_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.prospectuses[2] = Some(file);
                    Ok(())
                }
                // 发行保荐书, 申报稿
                (Some(36), Some(1)) => {
                    file.path.push(DECLARE_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.publish_sponsor[0] = Some(file);
                    Ok(())
                }
                // 发行保荐书, 上会稿
                (Some(36), Some(2)) => {
                    file.path.push(MEETING_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.publish_sponsor[1] = Some(file);
                    Ok(())
                }
                // 发行保荐书, 注册稿
                (Some(36), Some(3)) => {
                    file.path.push(REGISTER_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.publish_sponsor[2] = Some(file);
                    Ok(())
                }
                // 上市保荐书, 申报稿
                (Some(37), Some(1)) => {
                    file.path.push(DECLARE_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.list_sponsor[0] = Some(file);
                    Ok(())
                }
                // 上市保荐书, 上会稿
                (Some(37), Some(2)) => {
                    file.path.push(MEETING_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.list_sponsor[1] = Some(file);
                    Ok(())
                }
                // 上市保荐书, 注册稿
                (Some(37), Some(3)) => {
                    file.path.push(REGISTER_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.list_sponsor[2] = Some(file);
                    Ok(())
                }
                // 审计报告, 申报稿
                (Some(32), Some(1)) => {
                    file.path.push(DECLARE_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.audit_report[0] = Some(file);
                    Ok(())
                }
                // 审计报告, 上会稿
                (Some(32), Some(2)) => {
                    file.path.push(MEETING_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.audit_report[1] = Some(file);
                    Ok(())
                }
                // 审计报告, 注册稿
                (Some(32), Some(3)) => {
                    file.path.push(REGISTER_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.audit_report[2] = Some(file);
                    Ok(())
                }
                // 法律意见书, 申报稿
                (Some(33), Some(1)) => {
                    file.path.push(DECLARE_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.legal_opinion[0] = Some(file);
                    Ok(())
                }
                // 法律意见书, 上会稿
                (Some(33), Some(2)) => {
                    file.path.push(MEETING_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.legal_opinion[1] = Some(file);
                    Ok(())
                }
                // 法律意见书, 注册稿
                (Some(33), Some(3)) => {
                    file.path.push(REGISTER_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.legal_opinion[2] = Some(file);
                    Ok(())
                }
                // 其他, 申报稿
                (Some(34), Some(1)) => {
                    file.path.push(DECLARE_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.others[0] = Some(file);
                    Ok(())
                }
                // 其他, 上会稿
                (Some(34), Some(2)) => {
                    file.path.push(MEETING_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.others[1] = Some(file);
                    Ok(())
                }
                // 其他, 注册稿
                (Some(34), Some(3)) => {
                    file.path.push(REGISTER_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.others[2] = Some(file);
                    Ok(())
                }
                // 问询和回复
                (Some(5) | Some(6), _) => {
                    // 发行人及保荐机构
                    if file.filename.starts_with("8-1") {
                        file.path.push(SPONSOR_SUBFOLDER);
                        file.path.push(&file.filename);
                        file.path.set_extension("pdf");
                        infos.query_and_reply.push(Some(QueryReply::Sponsor(file)));
                    } else if file.filename.starts_with("8-2") {
                        // 会计师
                        file.path.push(ACCOUNTANT_SUBFOLDER);
                        file.path.push(&file.filename);
                        file.path.set_extension("pdf");
                        infos
                            .query_and_reply
                            .push(Some(QueryReply::Accountant(file)));
                    } else if file.filename.starts_with("8-3") {
                        // 律师
                        file.path.push(LAWYER_SUBFOLDER);
                        file.path.push(&file.filename);
                        file.path.set_extension("pdf");
                        infos.query_and_reply.push(Some(QueryReply::Lawyer(file)));
                    } else {
                        file.path.push("问询与回复");
                        file.path.push(&file.filename);
                        file.path.set_extension("pdf");
                        infos.query_and_reply.push(Some(QueryReply::Other(file)));
                    }
                    Ok(())
                }
                // 注册结果通知和终止审核通知
                (Some(35) | Some(38), _) => {
                    file.path.push(RESULT_SUBFOLDER);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.register_result_or_audit_terminated.push(Some(file));
                    Ok(())
                }
                // other
                (_, _) => Err(anyhow!("unknown file type")),
            }
        });
        if ret.is_ok() {
            Ok(infos)
        } else {
            Err(anyhow!("Error in parsing file path for InfoDisclosure"))
        }
    }
}

/// 上市委会议公告与结果
#[derive(Debug, Default)]
pub struct MeetingAnnounce {
    announcements: Vec<Option<UploadFile>>,
}

impl TryFrom<String> for MeetingAnnounce {
    type Error = anyhow::Error;
    fn try_from(resp: String) -> Result<Self, Self::Error> {
        #[allow(clippy::useless_format)]
        let json_str = format!(
            r#"{}"#,
            resp.split_terminator(&['(', ')'][..])
                .next_back()
                .context("invalid input")?
        );
        let json_body: Value = serde_json::from_str(&json_str)?;
        let mut announce = MeetingAnnounce::default();
        let file_arr = json_body["result"]
            .as_array()
            .context("extract file array failed")?;
        let download_base = Url::parse("http://static.sse.com.cn/stock")?;
        let ret: Result<(), anyhow::Error> = file_arr.iter().try_for_each(|x| {
            let file = UploadFile {
                filename: {
                    let name = x["fileTitle"].as_str().context("get filename failed")?;
                    name.to_owned()
                },
                url: {
                    let download_url = x["filePath"].as_str().context("get file url failed")?;
                    download_base.join(download_url)?
                },
                path: {
                    let company_name = x["stockAudit"][0]["companyFullName"]
                        .as_str()
                        .context("get company name failed")?;
                    let mut path = PathBuf::new();
                    path.push("Download");
                    path.push(company_name);
                    path.push(RESULT_SUBFOLDER);
                    path.push(x["fileTitle"].as_str().unwrap());
                    path.set_extension("pdf");
                    path
                },
            };
            announce.announcements.push(Some(file));
            Ok(())
        });
        if ret.is_ok() {
            Ok(announce)
        } else {
            Err(anyhow!("Error in parsing path for MeetingAnnounce"))
        }
    }
}

/// 公司信息汇总
#[derive(Debug)]
pub struct ItemDetail {
    overview: CompanyInfo,
    disclosure: InfoDisclosure,
    announce: MeetingAnnounce,
}

/// 爬虫入口
#[derive(Debug)]
pub struct SseCrawler {
    // reqwest client
    client: Client,
    // 所有公司信息
    companies: Vec<ItemDetail>,
    // 出错的公司名字，需人工处理
    failed_logs: Vec<String>,
}

impl SseCrawler {
    pub fn new() -> Self {
        let mut headers = header::HeaderMap::new();
        headers.insert("User-Agent", header::HeaderValue::from_static("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/96.0.4664.93 Safari/537.36"));
        headers.insert(
            "Referer",
            header::HeaderValue::from_static("https://kcb.sse.com.cn"),
        );
        let client = reqwest::Client::builder()
            .cookie_store(true)
            .default_headers(headers)
            .build()
            .unwrap();
        Self {
            client,
            companies: Vec::new(),
            failed_logs: Vec::new(),
        }
    }

    async fn query_company_overview(&self, name: &str) -> Result<CompanyInfo, Error> {
        let url = format!("http://query.sse.com.cn/statusAction.do?jsonCallBack=jsonpCallback42305&isPagination=true&sqlId=SH_XM_LB&pageHelp.pageSize=20&offerType=&commitiResult=&registeResult=&province=&csrcCode=&currStatus=&order=&keyword={}&auditApplyDateBegin=&auditApplyDateEnd=&_=1640867539069", name);
        let resp = self.client.get(url).send().await?;

        let body = resp.text().await?;
        Ok(CompanyInfo::try_from(body)?)
    }

    async fn query_company_disclosure(&self, id: u32) -> Result<InfoDisclosure, Error> {
        let url = format!("http://query.sse.com.cn/commonSoaQuery.do?jsonCallBack=jsonpCallback99435173&isPagination=false&sqlId=GP_GPZCZ_SHXXPL&stockAuditNum={}&_=1641094982593", id);
        let resp = self.client.get(url).send().await?;

        let body = resp.text().await?;
        Ok(InfoDisclosure::try_from(body)?)
    }

    async fn query_company_announce(&self, id: u32) -> Result<MeetingAnnounce, Error> {
        let url = format!("http://query.sse.com.cn/commonSoaQuery.do?jsonCallBack=jsonpCallback42495292&isPagination=false&sqlId=GP_GPZCZ_SSWHYGGJG&fileType=1,2,3,4&stockAuditNum={}&_=1641114627446", id);
        let resp = self.client.get(url).send().await?;

        let body = resp.text().await?;
        Ok(MeetingAnnounce::try_from(body)?)
    }

    pub async fn process_company(&mut self, name: &str) {
        let mut audit_id: u32 = 0;
        let company_info = self.query_company_overview(name).await;
        if company_info.is_ok() {
            audit_id = company_info.as_ref().unwrap().stock_audit_number;
            let disclosure = self.query_company_disclosure(audit_id).await;
            let announce = self.query_company_announce(audit_id).await;
            if disclosure.is_ok() && announce.is_ok() {
                self.companies.push(ItemDetail {
                    overview: company_info.unwrap(),
                    disclosure: disclosure.unwrap(),
                    announce: announce.unwrap(),
                })
            }
        } else {
            self.failed_logs.push(name.to_owned())
        }
    }

    pub async fn download_company_files(&self, company: &ItemDetail) -> anyhow::Result<()> {
        let base_folder = &company.overview.stock_audit_name;

        // create SUBFOLDERS to save pdf files
        SUBFOLDERS.map(|folder| {
            let sub_folder: PathBuf = ["Download", base_folder, folder]
                .iter()
                .collect::<PathBuf>();
            std::fs::create_dir_all(sub_folder)
                .unwrap_or_else(|why| println!("! {:?}", why.kind()));
        });

        let mut download_tasks = Vec::<(&Url, &PathBuf)>::new();
        company.disclosure.prospectuses.iter().for_each(|x| {
            if x.is_some() {
                let y = x.as_ref().unwrap();
                download_tasks.push((&y.url, &y.path));
            }
        });
        company.disclosure.publish_sponsor.iter().for_each(|x| {
            if x.is_some() {
                let y = x.as_ref().unwrap();
                download_tasks.push((&y.url, &y.path));
            }
        });
        company.disclosure.list_sponsor.iter().for_each(|x| {
            if x.is_some() {
                let y = x.as_ref().unwrap();
                download_tasks.push((&y.url, &y.path));
            }
        });
        company.disclosure.audit_report.iter().for_each(|x| {
            if x.is_some() {
                let y = x.as_ref().unwrap();
                download_tasks.push((&y.url, &y.path));
            }
        });
        company.disclosure.legal_opinion.iter().for_each(|x| {
            if x.is_some() {
                let y = x.as_ref().unwrap();
                download_tasks.push((&y.url, &y.path));
            }
        });
        company.disclosure.others.iter().for_each(|x| {
            if x.is_some() {
                let y = x.as_ref().unwrap();
                download_tasks.push((&y.url, &y.path));
            }
        });
        company.disclosure.query_and_reply.iter().for_each(|x| {
            let y = x.as_ref().unwrap();
            match y {
                QueryReply::Sponsor(z) => download_tasks.push((&z.url, &z.path)),
                QueryReply::Accountant(z) => download_tasks.push((&z.url, &z.path)),
                QueryReply::Lawyer(z) => download_tasks.push((&z.url, &z.path)),
                QueryReply::Other(z) => download_tasks.push((&z.url, &z.path)),
            }
        });
        company
            .disclosure
            .register_result_or_audit_terminated
            .iter()
            .for_each(|x| {
                let y = x.as_ref().unwrap();
                download_tasks.push((&y.url, &y.path));
            });
        company.announce.announcements.iter().for_each(|x| {
            let y = x.as_ref().unwrap();
            download_tasks.push((&y.url, &y.path));
        });
        println!("{:#?}", download_tasks);
        for (url, path) in download_tasks {
            let url_copy = url.clone().into_string();
            // download_tasks.iter().try_for_each(|(&url, &path)| {
            let resp = self.client.get(url_copy).send().await?;
            let mut content = resp.bytes().await?;
            let mut file = std::fs::File::create(path)?;
            file.write_all(&*content)?;
            // std::io::copy(&mut content, &mut file);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_store_cookie_automatically() {
        let mut headers = header::HeaderMap::new();
        headers.insert("User-Agent", header::HeaderValue::from_static("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/96.0.4664.93 Safari/537.36"));
        headers.insert(
            "Referer",
            header::HeaderValue::from_static("https://kcb.sse.com.cn"),
        );
        let client = reqwest::Client::builder()
            .cookie_store(true)
            .default_headers(headers)
            .build()
            .unwrap();
        let mut res = client
            .get("http://kcb.sse.com.cn/renewal")
            .send()
            .await
            .unwrap();
        res = client.get("http://query.sse.com.cn/commonSoaQuery.do?jsonCallBack=jsonpCallback42916568&isPagination=false&sqlId=GP_GPZCZ_SHXXPL&stockAuditNum=961&_=1640614222583")
            .send()
            .await
            .unwrap();

        let mut body = res.text().await.unwrap();
        // let body =res.json::<HashMap<String, String>>()
        // .await.unwrap();
        println!("{:?}", body);
        println!("{:?}", client);
    }

    #[tokio::test]
    async fn test_query_company_brief() {
        let mut headers = header::HeaderMap::new();
        headers.insert("User-Agent", header::HeaderValue::from_static("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/96.0.4664.93 Safari/537.36"));
        headers.insert(
            "Referer",
            header::HeaderValue::from_static("https://kcb.sse.com.cn"),
        );
        let client = reqwest::Client::builder()
            .cookie_store(true)
            .default_headers(headers)
            .build()
            .unwrap();
        let mut res = client
            .get("http://kcb.sse.com.cn/renewal")
            .send()
            .await
            .unwrap();
        let url = format!("http://query.sse.com.cn/statusAction.do?jsonCallBack=jsonpCallback42305&isPagination=true&sqlId=SH_XM_LB&pageHelp.pageSize=20&offerType=&commitiResult=&registeResult=&province=&csrcCode=&currStatus=&order=&keyword={}&auditApplyDateBegin=&auditApplyDateEnd=&_=1640867539069","北京英诺特生物技术股份有限公司");
        res = client.get(url).send().await.unwrap();

        // let mut body = res.text().await.unwrap();
        let body = res.text().await.unwrap();

        let json_str = format!(
            r#"{}"#,
            body.split_terminator(&['(', ')'][..]).next_back().unwrap()
        );
        let json: Value = serde_json::from_str(&json_str).unwrap();
        println!("{:#?}", json);
        // println!("{:?}", client);
    }

    #[test]
    fn test_company_info_try_from_json() {
        let raw_data = String::from(
            r#"jsonpCallback85975({"actionErrors":[],"actionMessages":[],"downloadFileName":null,"execlStream":null,"fieldErrors":{},"fileId":null,"isPagination":"true","jsonCallBack":"jsonpCallback85975","locale":"zh_CN","mergeSize":null,"mergeType":null,"pageHelp":{"beginPage":1,"cacheSize":1,"data":[{"updateDate":"20211230191852","planIssueCapital":12.09,"suspendStatus":"","wenHao":"","stockAuditName":"北京英诺特生物技术股份有限公司","currStatus":2,"stockAuditNum":"924","registeResult":"","intermediary":[{"auditId":"924","i_intermediaryType":1,"i_intermediaryId":"0_1047","i_person":[{"i_p_personName":"董炜源","i_p_jobType":24,"i_p_personId":"787524","i_p_jobTitle":"项目协办人"},{"i_p_personName":"郑明欣","i_p_jobType":23,"i_p_personId":"787523","i_p_jobTitle":"保荐代表人B"},{"i_p_personName":"丁明明","i_p_jobType":22,"i_p_personId":"787522","i_p_jobTitle":"保荐代表人A"},{"i_p_personName":"唐松华","i_p_jobType":21,"i_p_personId":"787519","i_p_jobTitle":"保荐业务负责人"}],"i_intermediaryAbbrName":"华泰联合证券","i_intermediaryName":"华泰联合证券有限责任公司","i_intermediaryOrder":1},{"auditId":"924","i_intermediaryType":2,"i_intermediaryId":"20008","i_person":[{"i_p_personName":"胡咏华","i_p_jobType":31,"i_p_personId":"787537","i_p_jobTitle":"会计事务所负责人"},{"i_p_personName":"丁亭亭","i_p_jobType":34,"i_p_personId":"787540","i_p_jobTitle":"签字会计师C（或有）"},{"i_p_personName":"岑溯鹏","i_p_jobType":33,"i_p_personId":"787539","i_p_jobTitle":"签字会计师B"},{"i_p_personName":"牛良文","i_p_jobType":32,"i_p_personId":"787538","i_p_jobTitle":"签字会计师A"}],"i_intermediaryAbbrName":"大信","i_intermediaryName":"大信会计师事务所（特殊普通合伙）","i_intermediaryOrder":1},{"auditId":"924","i_intermediaryType":3,"i_intermediaryId":"10006","i_person":[{"i_p_personName":"魏海涛","i_p_jobType":42,"i_p_personId":"787543","i_p_jobTitle":"签字律师A"},{"i_p_personName":"姚启明","i_p_jobType":43,"i_p_personId":"787544","i_p_jobTitle":"签字律师B"},{"i_p_personName":"张学兵","i_p_jobType":41,"i_p_personId":"787542","i_p_jobTitle":"律师事务所负责人"},{"i_p_personName":"丁文昊","i_p_jobType":44,"i_p_personId":"787545","i_p_jobTitle":"签字律师C（或有）"}],"i_intermediaryAbbrName":"北京市中伦","i_intermediaryName":"北京市中伦律师事务所","i_intermediaryOrder":1}],"collectType":1,"stockIssuer":[{"s_personName":"叶逢光","auditId":"924","s_stockIssueId":"924","s_personId":"787515","s_issueCompanyFullName":"北京英诺特生物技术股份有限公司","s_csrcCode":"C27","s_jobTitle":"董事长","s_issueCompanyAbbrName":"英诺特","s_csrcCodeDesc":"医药制造业","s_province":"北京","s_areaNameDesc":"丰台区","s_companyCode":""},{"s_personName":"张秀杰","auditId":"924","s_stockIssueId":"924","s_personId":"787516","s_issueCompanyFullName":"北京英诺特生物技术股份有限公司","s_csrcCode":"C27","s_jobTitle":"总经理","s_issueCompanyAbbrName":"英诺特","s_csrcCodeDesc":"医药制造业","s_province":"北京","s_areaNameDesc":"丰台区","s_companyCode":""},{"s_personName":"陈富康","auditId":"924","s_stockIssueId":"924","s_personId":"787517","s_issueCompanyFullName":"北京英诺特生物技术股份有限公司","s_csrcCode":"C27","s_jobTitle":"董秘","s_issueCompanyAbbrName":"英诺特","s_csrcCodeDesc":"医药制造业","s_province":"北京","s_areaNameDesc":"丰台区","s_companyCode":""},{"s_personName":"","auditId":"924","s_stockIssueId":"924","s_personId":"787518","s_issueCompanyFullName":"北京英诺特生物技术股份有限公司","s_csrcCode":"C27","s_jobTitle":"其他","s_issueCompanyAbbrName":"英诺特","s_csrcCodeDesc":"医药制造业","s_province":"北京","s_areaNameDesc":"丰台区","s_companyCode":""}],"createTime":"20210607144024","auditApplyDate":"20210616165743","issueAmount":"","commitiResult":"","issueMarketType":1,"OPERATION_SEQ":"17091ec3adcced13ef280fdbf3c35881"}],"endDate":null,"endPage":null,"objectResult":null,"pageCount":1,"pageNo":1,"pageSize":20,"searchDate":null,"sort":null,"startDate":null,"total":1},"pageNo":null,"pageSize":null,"queryDate":"","result":[{"updateDate":"20211230191852","planIssueCapital":12.09,"suspendStatus":"","wenHao":"","stockAuditName":"北京英诺特生物技术股份有限公司","currStatus":2,"stockAuditNum":"924","registeResult":"","intermediary":[{"auditId":"924","i_intermediaryType":1,"i_intermediaryId":"0_1047","i_person":[{"i_p_personName":"董炜源","i_p_jobType":24,"i_p_personId":"787524","i_p_jobTitle":"项目协办人"},{"i_p_personName":"郑明欣","i_p_jobType":23,"i_p_personId":"787523","i_p_jobTitle":"保荐代表人B"},{"i_p_personName":"丁明明","i_p_jobType":22,"i_p_personId":"787522","i_p_jobTitle":"保荐代表人A"},{"i_p_personName":"唐松华","i_p_jobType":21,"i_p_personId":"787519","i_p_jobTitle":"保荐业务负责人"}],"i_intermediaryAbbrName":"华泰联合证券","i_intermediaryName":"华泰联合证券有限责任公司","i_intermediaryOrder":1},{"auditId":"924","i_intermediaryType":2,"i_intermediaryId":"20008","i_person":[{"i_p_personName":"胡咏华","i_p_jobType":31,"i_p_personId":"787537","i_p_jobTitle":"会计事务所负责人"},{"i_p_personName":"丁亭亭","i_p_jobType":34,"i_p_personId":"787540","i_p_jobTitle":"签字会计师C（或有）"},{"i_p_personName":"岑溯鹏","i_p_jobType":33,"i_p_personId":"787539","i_p_jobTitle":"签字会计师B"},{"i_p_personName":"牛良文","i_p_jobType":32,"i_p_personId":"787538","i_p_jobTitle":"签字会计师A"}],"i_intermediaryAbbrName":"大信","i_intermediaryName":"大信会计师事务所（特殊普通合伙）","i_intermediaryOrder":1},{"auditId":"924","i_intermediaryType":3,"i_intermediaryId":"10006","i_person":[{"i_p_personName":"魏海涛","i_p_jobType":42,"i_p_personId":"787543","i_p_jobTitle":"签字律师A"},{"i_p_personName":"姚启明","i_p_jobType":43,"i_p_personId":"787544","i_p_jobTitle":"签字律师B"},{"i_p_personName":"张学兵","i_p_jobType":41,"i_p_personId":"787542","i_p_jobTitle":"律师事务所负责人"},{"i_p_personName":"丁文昊","i_p_jobType":44,"i_p_personId":"787545","i_p_jobTitle":"签字律师C（或有）"}],"i_intermediaryAbbrName":"北京市中伦","i_intermediaryName":"北京市中伦律师事务所","i_intermediaryOrder":1}],"collectType":1,"stockIssuer":[{"s_personName":"叶逢光","auditId":"924","s_stockIssueId":"924","s_personId":"787515","s_issueCompanyFullName":"北京英诺特生物技术股份有限公司","s_csrcCode":"C27","s_jobTitle":"董事长","s_issueCompanyAbbrName":"英诺特","s_csrcCodeDesc":"医药制造业","s_province":"北京","s_areaNameDesc":"丰台区","s_companyCode":""},{"s_personName":"张秀杰","auditId":"924","s_stockIssueId":"924","s_personId":"787516","s_issueCompanyFullName":"北京英诺特生物技术股份有限公司","s_csrcCode":"C27","s_jobTitle":"总经理","s_issueCompanyAbbrName":"英诺特","s_csrcCodeDesc":"医药制造业","s_province":"北京","s_areaNameDesc":"丰台区","s_companyCode":""},{"s_personName":"陈富康","auditId":"924","s_stockIssueId":"924","s_personId":"787517","s_issueCompanyFullName":"北京英诺特生物技术股份有限公司","s_csrcCode":"C27","s_jobTitle":"董秘","s_issueCompanyAbbrName":"英诺特","s_csrcCodeDesc":"医药制造业","s_province":"北京","s_areaNameDesc":"丰台区","s_companyCode":""},{"s_personName":"","auditId":"924","s_stockIssueId":"924","s_personId":"787518","s_issueCompanyFullName":"北京英诺特生物技术股份有限公司","s_csrcCode":"C27","s_jobTitle":"其他","s_issueCompanyAbbrName":"英诺特","s_csrcCodeDesc":"医药制造业","s_province":"北京","s_areaNameDesc":"丰台区","s_companyCode":""}],"createTime":"20210607144024","auditApplyDate":"20210616165743","issueAmount":"","commitiResult":"","issueMarketType":1,"OPERATION_SEQ":"17091ec3adcced13ef280fdbf3c35881"}],"securityCode":"","statistics":[{"num":31,"status":"1"},{"num":61,"status":"2"},{"num":405,"status":"5"},{"num":48,"status":"4"},{"num":145,"status":"8"},{"num":7,"status":"7"},{"num":12,"status":"3"},{"num":2,"status":"3-6"},{"num":10,"status":"3-1"},{"num":1,"status":"5-2"},{"num":390,"status":"5-1"},{"num":14,"status":"5-3"},{"num":6,"status":"7-1"},{"num":1,"status":"7-2"}],"texts":null,"type":"","validateCode":""})"#,
        );
        let company_info = CompanyInfo::try_from(raw_data);
        println!("{:#?}", company_info);
    }

    #[test]
    fn test_invalid_company_info_try_from_json() {
        let raw_data = String::from(
            r#"jsonpCallback70586({"actionErrors":[],"actionMessages":[],"downloadFileName":null,"execlStream":null,"fieldErrors":{},"fileId":null,"isPagination":"true","jsonCallBack":"jsonpCallback70586","locale":"zh_CN","mergeSize":null,"mergeType":null,"pageHelp":{"beginPage":0,"cacheSize":1,"data":[],"endDate":null,"endPage":null,"objectResult":null,"pageCount":0,"pageNo":1,"pageSize":20,"searchDate":null,"sort":null,"startDate":null,"total":0},"pageNo":null,"pageSize":null,"queryDate":"","result":[],"securityCode":"","statistics":[{"num":31,"status":"1"},{"num":61,"status":"2"},{"num":405,"status":"5"},{"num":48,"status":"4"},{"num":145,"status":"8"},{"num":7,"status":"7"},{"num":12,"status":"3"},{"num":2,"status":"3-6"},{"num":10,"status":"3-1"},{"num":1,"status":"5-2"},{"num":390,"status":"5-1"},{"num":14,"status":"5-3"},{"num":6,"status":"7-1"},{"num":1,"status":"7-2"}],"texts":null,"type":"","validateCode":""})"#,
        );
        let company_info = CompanyInfo::try_from(raw_data);
        println!("{:#?}", company_info);
    }

    #[test]
    fn test_info_disclosure_try_from_json() {
        let raw_data = String::from(
            r#"jsonpCallback36967625({"actionErrors":[],"actionMessages":[],"fieldErrors":{},"isPagination":"false","jsonCallBack":"jsonpCallback36967625","locale":"zh_CN","pageHelp":{"beginPage":1,"cacheSize":1,"data":[{"fileUpdateTime":"20211231173000","filePath":"\/information\/c\/202112\/61212b6e7f1d41758ec796c635dca875.pdf","publishDate":"2021-12-31","fileLastVersion":1,"stockAuditNum":"942","auditItemId":"39ea19626a1c11ec9f2608f1ea8a847f","filename":"61212b6e7f1d41758ec796c635dca875.pdf","companyFullName":"大汉软件股份有限公司","fileSize":309224,"StockType":1,"fileTitle":"关于终止对大汉软件股份有限公司首次公开发行股票并在科创板上市审核的决定","auditStatus":8,"fileVersion":4,"fileType":38,"MarketType":1,"fileId":"223439","OPERATION_SEQ":"9239c490b4d887502a2c4204d6f2c1a9"},{"fileUpdateTime":"20211230170001","filePath":"\/information\/c\/202112\/0b7a118a34d94416b003df304ec365ad.pdf","publishDate":"2021-12-30","fileLastVersion":1,"stockAuditNum":"942","auditItemId":"3bd89c5c3be741dc9c5b7bd317e13393","filename":"0b7a118a34d94416b003df304ec365ad.pdf","companyFullName":"大汉软件股份有限公司","fileSize":1939743,"StockType":1,"fileTitle":"8-1-2 发行人及保荐机构关于第二轮审核问询函的回复","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"83950911","OPERATION_SEQ":"69a4517cf2253e13fc2d8a3e4db5a5af"},{"fileUpdateTime":"20211230170001","filePath":"\/information\/c\/202112\/2c78f1add94544fb985e5d04fced3a0c.pdf","publishDate":"2021-12-30","fileLastVersion":1,"stockAuditNum":"942","auditItemId":"2bc9db5a71cd414dabf497a2c73689ee","filename":"2c78f1add94544fb985e5d04fced3a0c.pdf","companyFullName":"大汉软件股份有限公司","fileSize":943527,"StockType":1,"fileTitle":"8-2-2 申报会计师关于大汉软件股份有限公司首次公开发行股票并在科创板上市申请文件的审核第二轮问询函回复的专项说明","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"83950910","OPERATION_SEQ":"69a4517cf2253e13fc2d8a3e4db5a5af"},{"fileUpdateTime":"20211230170001","filePath":"\/information\/c\/202112\/299ccaf24da44dde99b47e85079cd886.pdf","publishDate":"2021-12-30","fileLastVersion":1,"stockAuditNum":"942","auditItemId":"51608979379d45688e864cfe57203b34","filename":"299ccaf24da44dde99b47e85079cd886.pdf","companyFullName":"大汉软件股份有限公司","fileSize":881602,"StockType":1,"fileTitle":"8-3 补充法律意见书（二）","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"83950909","OPERATION_SEQ":"69a4517cf2253e13fc2d8a3e4db5a5af"},{"fileUpdateTime":"20211224183000","filePath":"\/information\/c\/202112\/545b1ae7d78a47f894ee1a7058a76a52.pdf","publishDate":"2021-12-24","fileLastVersion":1,"stockAuditNum":"942","auditItemId":"7bf0a56a66e2490182ddcc9d55ddb6c9","filename":"545b1ae7d78a47f894ee1a7058a76a52.pdf","companyFullName":"大汉软件股份有限公司","fileSize":3174990,"StockType":1,"fileTitle":"8-1-1 发行人及保荐机构关于第一轮审核问询函的回复（2021年半年报财务数据更新版）","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"83900160","OPERATION_SEQ":"7b77b93262e7704ea174e30c28485d00"},{"fileUpdateTime":"20211224183000","filePath":"\/information\/c\/202112\/e0d096f9e6a547dbb13e857a659db3d2.pdf","publishDate":"2021-12-24","fileLastVersion":1,"stockAuditNum":"942","auditItemId":"d58f6c72947a4300bd3b0b7f63d411e3","filename":"e0d096f9e6a547dbb13e857a659db3d2.pdf","companyFullName":"大汉软件股份有限公司","fileSize":1855841,"StockType":1,"fileTitle":"8-2-1 申报会计师关于大汉软件股份有限公司首次公开发行股票并在科创板上市申请文件的审核问询函回复的专项说明（2021年半年报财务数据更新版）","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"83900159","OPERATION_SEQ":"7b77b93262e7704ea174e30c28485d00"},{"fileUpdateTime":"20211224183000","filePath":"\/information\/c\/202112\/7387c9dfea284f769fb2c40c0fece3b9.pdf","publishDate":"2021-12-24","fileLastVersion":1,"stockAuditNum":"942","auditItemId":"72cc681064a411ec9f2608f1ea8a847f","filename":"7387c9dfea284f769fb2c40c0fece3b9.pdf","companyFullName":"大汉软件股份有限公司","fileSize":877386,"StockType":1,"fileTitle":"3-3-1 补充法律意见书（二）","auditStatus":2,"fileVersion":1,"fileType":33,"MarketType":1,"fileId":"221379","OPERATION_SEQ":"7b77b93262e7704ea174e30c28485d00"},{"fileUpdateTime":"20210914170500","filePath":"\/information\/c\/202109\/2c4614c50a294572936dae91c8dbee68.pdf","publishDate":"2021-09-14","fileLastVersion":1,"stockAuditNum":"942","auditItemId":"224291a1b9334d46bf8ed1039cd35b72","filename":"2c4614c50a294572936dae91c8dbee68.pdf","companyFullName":"大汉软件股份有限公司","fileSize":5391111,"StockType":1,"fileTitle":"8-1 发行人及保荐机构关于大汉软件股份有限公司首次公开发行股票并在科创板上市申请文件的审核问询函的回复","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"82999614","OPERATION_SEQ":"250d0dd20ea2a2708595504196ff4a38"},{"fileUpdateTime":"20210914170500","filePath":"\/information\/c\/202109\/1915217e59d84bcd913c37a512591ff3.pdf","publishDate":"2021-09-14","fileLastVersion":1,"stockAuditNum":"942","auditItemId":"b02679d1f25d4734aeaa244e0b70f5e5","filename":"1915217e59d84bcd913c37a512591ff3.pdf","companyFullName":"大汉软件股份有限公司","fileSize":698723,"StockType":1,"fileTitle":"8-3 发行人律师出具的补充法律意见书（一）","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"82999613","OPERATION_SEQ":"250d0dd20ea2a2708595504196ff4a38"},{"fileUpdateTime":"20210914170500","filePath":"\/information\/c\/202109\/dd03d18d08944585b55fd92a50c1f401.pdf","publishDate":"2021-09-14","fileLastVersion":1,"stockAuditNum":"942","auditItemId":"87e12387d73e4671a7c3ba802b5eb18a","filename":"dd03d18d08944585b55fd92a50c1f401.pdf","companyFullName":"大汉软件股份有限公司","fileSize":2384924,"StockType":1,"fileTitle":"8-2 毕马威华振会计师事务所（特殊普通合伙）关于大汉软件股份有限公司首次公开发行股票并在科创板上市申请文件的审核问询函回复的专项说明","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"82999612","OPERATION_SEQ":"250d0dd20ea2a2708595504196ff4a38"},{"fileUpdateTime":"20210630170004","filePath":"\/information\/c\/202106\/7d09ce43a4074a4ebeacfb2936e30b56.pdf","publishDate":"2021-06-30","fileLastVersion":2,"stockAuditNum":"942","auditItemId":"8f4de27fd98111eb9f2608f1ea8a847f","filename":"7d09ce43a4074a4ebeacfb2936e30b56.pdf","companyFullName":"大汉软件股份有限公司","fileSize":845627,"StockType":1,"fileTitle":"上海市锦天城律师事务所关于大汉软件股份公司首次公开发行股票并在科创板上市的法律意见书","auditStatus":1,"fileVersion":1,"fileType":33,"MarketType":1,"fileId":"195556","OPERATION_SEQ":"dac202d102ab3f91adf722ce44d12f8a"},{"fileUpdateTime":"20210630170004","filePath":"\/information\/c\/202106\/158b620a0bf145ae9676c00ecbf32a02.pdf","publishDate":"2021-06-30","fileLastVersion":2,"stockAuditNum":"942","auditItemId":"8f4de27fd98111eb9f2608f1ea8a847f","filename":"158b620a0bf145ae9676c00ecbf32a02.pdf","companyFullName":"大汉软件股份有限公司","fileSize":9371096,"StockType":1,"fileTitle":"毕马威华振会计师事务所（特殊普通合伙）关于大汉软件股份公司首次公开发行股票并在科创板上市的财务报表及审计报告","auditStatus":1,"fileVersion":1,"fileType":32,"MarketType":1,"fileId":"195552","OPERATION_SEQ":"dac202d102ab3f91adf722ce44d12f8a"},{"fileUpdateTime":"20210630170004","filePath":"\/information\/c\/202106\/1b287b654b2a403eae3e9dbc601c9637.pdf","publishDate":"2021-06-30","fileLastVersion":2,"stockAuditNum":"942","auditItemId":"8f4de27fd98111eb9f2608f1ea8a847f","filename":"1b287b654b2a403eae3e9dbc601c9637.pdf","companyFullName":"大汉软件股份有限公司","fileSize":1264237,"StockType":1,"fileTitle":"安信证券股份有限公司关于大汉软件股份公司首次公开发行股票并在科创板上市的上市保荐书","auditStatus":1,"fileVersion":1,"fileType":37,"MarketType":1,"fileId":"195546","OPERATION_SEQ":"dac202d102ab3f91adf722ce44d12f8a"},{"fileUpdateTime":"20210630170004","filePath":"\/information\/c\/202106\/48fec1eb0ad64a2cba146f6f2a74dcee.pdf","publishDate":"2021-06-30","fileLastVersion":2,"stockAuditNum":"942","auditItemId":"8f4de27fd98111eb9f2608f1ea8a847f","filename":"48fec1eb0ad64a2cba146f6f2a74dcee.pdf","companyFullName":"大汉软件股份有限公司","fileSize":1218337,"StockType":1,"fileTitle":"安信证券股份有限公司关于大汉软件股份公司首次公开发行股票并在科创板上市的发行保荐书","auditStatus":1,"fileVersion":1,"fileType":36,"MarketType":1,"fileId":"195544","OPERATION_SEQ":"dac202d102ab3f91adf722ce44d12f8a"},{"fileUpdateTime":"20210630170004","filePath":"\/information\/c\/202106\/0cb8ee1d3f4549a09458ac08f7290504.pdf","publishDate":"2021-06-30","fileLastVersion":2,"stockAuditNum":"942","auditItemId":"8f4de27fd98111eb9f2608f1ea8a847f","filename":"0cb8ee1d3f4549a09458ac08f7290504.pdf","companyFullName":"大汉软件股份有限公司","fileSize":10764426,"StockType":1,"fileTitle":"大汉软件股份有限公司科创板首次公开发行股票招股说明书（申报稿）","auditStatus":1,"fileVersion":1,"fileType":30,"MarketType":1,"fileId":"195537","OPERATION_SEQ":"dac202d102ab3f91adf722ce44d12f8a"}],"endDate":null,"endPage":null,"objectResult":null,"pageCount":1,"pageNo":1,"pageSize":15,"searchDate":null,"sort":null,"startDate":null,"total":15},"pageNo":null,"pageSize":null,"queryDate":"","result":[{"fileUpdateTime":"20211231173000","filePath":"\/information\/c\/202112\/61212b6e7f1d41758ec796c635dca875.pdf","publishDate":"2021-12-31","fileLastVersion":1,"stockAuditNum":"942","auditItemId":"39ea19626a1c11ec9f2608f1ea8a847f","filename":"61212b6e7f1d41758ec796c635dca875.pdf","companyFullName":"大汉软件股份有限公司","fileSize":309224,"StockType":1,"fileTitle":"关于终止对大汉软件股份有限公司首次公开发行股票并在科创板上市审核的决定","auditStatus":8,"fileVersion":4,"fileType":38,"MarketType":1,"fileId":"223439","OPERATION_SEQ":"9239c490b4d887502a2c4204d6f2c1a9"},{"fileUpdateTime":"20211230170001","filePath":"\/information\/c\/202112\/0b7a118a34d94416b003df304ec365ad.pdf","publishDate":"2021-12-30","fileLastVersion":1,"stockAuditNum":"942","auditItemId":"3bd89c5c3be741dc9c5b7bd317e13393","filename":"0b7a118a34d94416b003df304ec365ad.pdf","companyFullName":"大汉软件股份有限公司","fileSize":1939743,"StockType":1,"fileTitle":"8-1-2 发行人及保荐机构关于第二轮审核问询函的回复","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"83950911","OPERATION_SEQ":"69a4517cf2253e13fc2d8a3e4db5a5af"},{"fileUpdateTime":"20211230170001","filePath":"\/information\/c\/202112\/2c78f1add94544fb985e5d04fced3a0c.pdf","publishDate":"2021-12-30","fileLastVersion":1,"stockAuditNum":"942","auditItemId":"2bc9db5a71cd414dabf497a2c73689ee","filename":"2c78f1add94544fb985e5d04fced3a0c.pdf","companyFullName":"大汉软件股份有限公司","fileSize":943527,"StockType":1,"fileTitle":"8-2-2 申报会计师关于大汉软件股份有限公司首次公开发行股票并在科创板上市申请文件的审核第二轮问询函回复的专项说明","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"83950910","OPERATION_SEQ":"69a4517cf2253e13fc2d8a3e4db5a5af"},{"fileUpdateTime":"20211230170001","filePath":"\/information\/c\/202112\/299ccaf24da44dde99b47e85079cd886.pdf","publishDate":"2021-12-30","fileLastVersion":1,"stockAuditNum":"942","auditItemId":"51608979379d45688e864cfe57203b34","filename":"299ccaf24da44dde99b47e85079cd886.pdf","companyFullName":"大汉软件股份有限公司","fileSize":881602,"StockType":1,"fileTitle":"8-3 补充法律意见书（二）","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"83950909","OPERATION_SEQ":"69a4517cf2253e13fc2d8a3e4db5a5af"},{"fileUpdateTime":"20211224183000","filePath":"\/information\/c\/202112\/545b1ae7d78a47f894ee1a7058a76a52.pdf","publishDate":"2021-12-24","fileLastVersion":1,"stockAuditNum":"942","auditItemId":"7bf0a56a66e2490182ddcc9d55ddb6c9","filename":"545b1ae7d78a47f894ee1a7058a76a52.pdf","companyFullName":"大汉软件股份有限公司","fileSize":3174990,"StockType":1,"fileTitle":"8-1-1 发行人及保荐机构关于第一轮审核问询函的回复（2021年半年报财务数据更新版）","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"83900160","OPERATION_SEQ":"7b77b93262e7704ea174e30c28485d00"},{"fileUpdateTime":"20211224183000","filePath":"\/information\/c\/202112\/e0d096f9e6a547dbb13e857a659db3d2.pdf","publishDate":"2021-12-24","fileLastVersion":1,"stockAuditNum":"942","auditItemId":"d58f6c72947a4300bd3b0b7f63d411e3","filename":"e0d096f9e6a547dbb13e857a659db3d2.pdf","companyFullName":"大汉软件股份有限公司","fileSize":1855841,"StockType":1,"fileTitle":"8-2-1 申报会计师关于大汉软件股份有限公司首次公开发行股票并在科创板上市申请文件的审核问询函回复的专项说明（2021年半年报财务数据更新版）","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"83900159","OPERATION_SEQ":"7b77b93262e7704ea174e30c28485d00"},{"fileUpdateTime":"20211224183000","filePath":"\/information\/c\/202112\/7387c9dfea284f769fb2c40c0fece3b9.pdf","publishDate":"2021-12-24","fileLastVersion":1,"stockAuditNum":"942","auditItemId":"72cc681064a411ec9f2608f1ea8a847f","filename":"7387c9dfea284f769fb2c40c0fece3b9.pdf","companyFullName":"大汉软件股份有限公司","fileSize":877386,"StockType":1,"fileTitle":"3-3-1 补充法律意见书（二）","auditStatus":2,"fileVersion":1,"fileType":33,"MarketType":1,"fileId":"221379","OPERATION_SEQ":"7b77b93262e7704ea174e30c28485d00"},{"fileUpdateTime":"20210914170500","filePath":"\/information\/c\/202109\/2c4614c50a294572936dae91c8dbee68.pdf","publishDate":"2021-09-14","fileLastVersion":1,"stockAuditNum":"942","auditItemId":"224291a1b9334d46bf8ed1039cd35b72","filename":"2c4614c50a294572936dae91c8dbee68.pdf","companyFullName":"大汉软件股份有限公司","fileSize":5391111,"StockType":1,"fileTitle":"8-1 发行人及保荐机构关于大汉软件股份有限公司首次公开发行股票并在科创板上市申请文件的审核问询函的回复","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"82999614","OPERATION_SEQ":"250d0dd20ea2a2708595504196ff4a38"},{"fileUpdateTime":"20210914170500","filePath":"\/information\/c\/202109\/1915217e59d84bcd913c37a512591ff3.pdf","publishDate":"2021-09-14","fileLastVersion":1,"stockAuditNum":"942","auditItemId":"b02679d1f25d4734aeaa244e0b70f5e5","filename":"1915217e59d84bcd913c37a512591ff3.pdf","companyFullName":"大汉软件股份有限公司","fileSize":698723,"StockType":1,"fileTitle":"8-3 发行人律师出具的补充法律意见书（一）","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"82999613","OPERATION_SEQ":"250d0dd20ea2a2708595504196ff4a38"},{"fileUpdateTime":"20210914170500","filePath":"\/information\/c\/202109\/dd03d18d08944585b55fd92a50c1f401.pdf","publishDate":"2021-09-14","fileLastVersion":1,"stockAuditNum":"942","auditItemId":"87e12387d73e4671a7c3ba802b5eb18a","filename":"dd03d18d08944585b55fd92a50c1f401.pdf","companyFullName":"大汉软件股份有限公司","fileSize":2384924,"StockType":1,"fileTitle":"8-2 毕马威华振会计师事务所（特殊普通合伙）关于大汉软件股份有限公司首次公开发行股票并在科创板上市申请文件的审核问询函回复的专项说明","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"82999612","OPERATION_SEQ":"250d0dd20ea2a2708595504196ff4a38"},{"fileUpdateTime":"20210630170004","filePath":"\/information\/c\/202106\/7d09ce43a4074a4ebeacfb2936e30b56.pdf","publishDate":"2021-06-30","fileLastVersion":2,"stockAuditNum":"942","auditItemId":"8f4de27fd98111eb9f2608f1ea8a847f","filename":"7d09ce43a4074a4ebeacfb2936e30b56.pdf","companyFullName":"大汉软件股份有限公司","fileSize":845627,"StockType":1,"fileTitle":"上海市锦天城律师事务所关于大汉软件股份公司首次公开发行股票并在科创板上市的法律意见书","auditStatus":1,"fileVersion":1,"fileType":33,"MarketType":1,"fileId":"195556","OPERATION_SEQ":"dac202d102ab3f91adf722ce44d12f8a"},{"fileUpdateTime":"20210630170004","filePath":"\/information\/c\/202106\/158b620a0bf145ae9676c00ecbf32a02.pdf","publishDate":"2021-06-30","fileLastVersion":2,"stockAuditNum":"942","auditItemId":"8f4de27fd98111eb9f2608f1ea8a847f","filename":"158b620a0bf145ae9676c00ecbf32a02.pdf","companyFullName":"大汉软件股份有限公司","fileSize":9371096,"StockType":1,"fileTitle":"毕马威华振会计师事务所（特殊普通合伙）关于大汉软件股份公司首次公开发行股票并在科创板上市的财务报表及审计报告","auditStatus":1,"fileVersion":1,"fileType":32,"MarketType":1,"fileId":"195552","OPERATION_SEQ":"dac202d102ab3f91adf722ce44d12f8a"},{"fileUpdateTime":"20210630170004","filePath":"\/information\/c\/202106\/1b287b654b2a403eae3e9dbc601c9637.pdf","publishDate":"2021-06-30","fileLastVersion":2,"stockAuditNum":"942","auditItemId":"8f4de27fd98111eb9f2608f1ea8a847f","filename":"1b287b654b2a403eae3e9dbc601c9637.pdf","companyFullName":"大汉软件股份有限公司","fileSize":1264237,"StockType":1,"fileTitle":"安信证券股份有限公司关于大汉软件股份公司首次公开发行股票并在科创板上市的上市保荐书","auditStatus":1,"fileVersion":1,"fileType":37,"MarketType":1,"fileId":"195546","OPERATION_SEQ":"dac202d102ab3f91adf722ce44d12f8a"},{"fileUpdateTime":"20210630170004","filePath":"\/information\/c\/202106\/48fec1eb0ad64a2cba146f6f2a74dcee.pdf","publishDate":"2021-06-30","fileLastVersion":2,"stockAuditNum":"942","auditItemId":"8f4de27fd98111eb9f2608f1ea8a847f","filename":"48fec1eb0ad64a2cba146f6f2a74dcee.pdf","companyFullName":"大汉软件股份有限公司","fileSize":1218337,"StockType":1,"fileTitle":"安信证券股份有限公司关于大汉软件股份公司首次公开发行股票并在科创板上市的发行保荐书","auditStatus":1,"fileVersion":1,"fileType":36,"MarketType":1,"fileId":"195544","OPERATION_SEQ":"dac202d102ab3f91adf722ce44d12f8a"},{"fileUpdateTime":"20210630170004","filePath":"\/information\/c\/202106\/0cb8ee1d3f4549a09458ac08f7290504.pdf","publishDate":"2021-06-30","fileLastVersion":2,"stockAuditNum":"942","auditItemId":"8f4de27fd98111eb9f2608f1ea8a847f","filename":"0cb8ee1d3f4549a09458ac08f7290504.pdf","companyFullName":"大汉软件股份有限公司","fileSize":10764426,"StockType":1,"fileTitle":"大汉软件股份有限公司科创板首次公开发行股票招股说明书（申报稿）","auditStatus":1,"fileVersion":1,"fileType":30,"MarketType":1,"fileId":"195537","OPERATION_SEQ":"dac202d102ab3f91adf722ce44d12f8a"}],"securityCode":"","texts":null,"type":"","validateCode":""})"#,
        );
        let disclosure = InfoDisclosure::try_from(raw_data);
        println!("{:#?}", disclosure);
    }

    #[tokio::test]
    async fn test_query_company_info() {
        let sse = SseCrawler::new();
        let company = sse
            .query_company_overview("大汉软件股份有限公司")
            .await
            .unwrap();
        println!("{:#?}", company)
    }

    #[tokio::test]
    async fn test_query_company_disclosure() {
        let sse = SseCrawler::new();
        let info = sse.query_company_disclosure(759).await.unwrap();
        println!("{:#?}", info)
    }

    #[tokio::test]
    async fn test_query_company_announce() {
        let sse = SseCrawler::new();
        let announce = sse.query_company_announce(759).await.unwrap();
        println!("{:#?}", announce)
    }

    #[tokio::test]
    async fn test_process_company() {
        let mut sse = SseCrawler::new();
        sse.process_company("大汉软件股份有限公司").await;
        println!("{:#?}", sse);
    }

    #[tokio::test]
    async fn test_process_more_companies() {
        let mut sse = Arc::new(Mutex::new(SseCrawler::new()));
        // let companies = ["上海赛伦生物技术股份有限公司", "大汉软件股份有限公司"];
        // for i in 0..companies.len() {
        //     sse.process_company(companies[i]).await;
        // }
        println!("{:#?}", sse);
    }

    #[tokio::test]
    async fn test_create_subfolder() {
        let mut sse = SseCrawler::new();
        sse.process_company("大汉软件股份有限公司").await;
        sse.download_company_files(&sse.companies[0]).await;
        // println!("{:#?}", sse);
    }
}
