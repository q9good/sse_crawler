use anyhow::{anyhow, Context, Error};
use reqwest::{header, Client, Url};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use time::macros::date;
use time::{format_description, Date, PrimitiveDateTime};
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};

static DECLARE_SUBFOLDER: &str = "申报稿";
static REGISTER_SUBFOLDER: &str = "注册稿";
static MEETING_SUBFOLDER: &str = "上会稿";
static SPONSOR_SUBFOLDER: &str = "问询与回复/发行人与保荐机构";
static ACCOUNTANT_SUBFOLDER: &str = "问询与回复/会计师";
static LAWYER_SUBFOLDER: &str = "问询与回复/律师";
static RESULT_SUBFOLDER: &str = "结果";
static UNCLASSIFIED_SUBFOLDER: &str = "问询与回复/未分组";

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
    Unsupported(u64, u64),
    Unknown,
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
        let pure_content: Vec<_> = resp.split_inclusive(&['(', ')'][..]).collect();
        #[allow(clippy::useless_format)]
        let mut json_str = format!(r#"{}"#, pure_content[1..].join(""));
        json_str.truncate(json_str.len() - 1);
        let json_body: Value = serde_json::from_str(&json_str)?;
        if matches!(&json_body["result"], Value::Array(result) if result.is_empty()) {
            return Err(anyhow!("empty company info"));
        }
        Ok(CompanyInfo {
            stock_audit_name: {
                // let company_name = json_body["result"][0]["stockAuditName"].as_str();
                let company_name =
                    json_body["result"][0]["stockIssuer"][0]["s_issueCompanyFullName"].as_str();
                if let Some(temp) = company_name {
                    temp.trim().to_owned()
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
                    (Some(s), Some(r)) => AuditStatus::Unsupported(s, r),
                    (_, _) => AuditStatus::Unknown,
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
    prospectuses: [Vec<UploadFile>; 3],
    // 发行保荐书
    publish_sponsor: [Vec<UploadFile>; 3],
    // 上市保荐书
    list_sponsor: [Vec<UploadFile>; 3],
    // 审计报告
    audit_report: [Vec<UploadFile>; 3],
    // 法律意见书
    legal_opinion: [Vec<UploadFile>; 3],
    // 其他
    others: [Vec<UploadFile>; 3],
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
        let pure_content: Vec<_> = resp.split_inclusive(&['(', ')'][..]).collect();
        #[allow(clippy::useless_format)]
        let mut json_str = format!(r#"{}"#, pure_content[1..].join(""));
        json_str.truncate(json_str.len() - 1);
        let json_body: Value = serde_json::from_str(&json_str)?;
        let mut infos = InfoDisclosure::default();
        let file_arr = json_body["result"]
            .as_array()
            .context("extract file array failed")?;
        let mut download_base = Url::parse("http://static.sse.com.cn/stock/")?;
        let ret = file_arr.iter().try_for_each(|x| {
            let date = x["publishDate"].as_str().context("get filename failed")?;
            let mut file = UploadFile {
                filename: {
                    let name = x["fileTitle"].as_str().context("get filename failed")?;
                    name.to_owned()
                },
                url: {
                    let download_url = x["filePath"].as_str().context("get file url failed")?;
                    download_base.set_path(&*("stock".to_owned() + download_url));
                    download_base.to_owned()
                },
                path: {
                    let mut path = PathBuf::new();
                    path.push("Download");
                    path.push(
                        x["companyFullName"]
                            .as_str()
                            .context("get company name failed")?
                            .trim(),
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
                    file.filename.push_str(date);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.prospectuses[0].push(file);
                    Ok(())
                }
                // 招股说明书, 上会稿
                (Some(30), Some(2)) => {
                    file.path.push(MEETING_SUBFOLDER);
                    file.filename.push_str(date);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.prospectuses[1].push(file);
                    Ok(())
                }
                // 招股说明书, 注册稿
                (Some(30), Some(3|4)) => {
                    file.path.push(REGISTER_SUBFOLDER);
                    file.filename.push_str(date);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.prospectuses[2].push(file);
                    Ok(())
                }
                // 发行保荐书, 申报稿
                (Some(36), Some(1)) => {
                    file.path.push(DECLARE_SUBFOLDER);
                    file.filename.push_str(date);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.publish_sponsor[0].push(file);
                    Ok(())
                }
                // 发行保荐书, 上会稿
                (Some(36), Some(2)) => {
                    file.path.push(MEETING_SUBFOLDER);
                    file.filename.push_str(date);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.publish_sponsor[1].push(file);
                    Ok(())
                }
                // 发行保荐书, 注册稿
                (Some(36), Some(3|4)) => {
                    file.path.push(REGISTER_SUBFOLDER);
                    file.filename.push_str(date);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.publish_sponsor[2].push(file);
                    Ok(())
                }
                // 上市保荐书, 申报稿
                (Some(37), Some(1)) => {
                    file.path.push(DECLARE_SUBFOLDER);
                    file.filename.push_str(date);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.list_sponsor[0].push(file);
                    Ok(())
                }
                // 上市保荐书, 上会稿
                (Some(37), Some(2)) => {
                    file.path.push(MEETING_SUBFOLDER);
                    file.filename.push_str(date);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.list_sponsor[1].push(file);
                    Ok(())
                }
                // 上市保荐书, 注册稿
                (Some(37), Some(3|4)) => {
                    file.path.push(REGISTER_SUBFOLDER);
                    file.filename.push_str(date);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.list_sponsor[2].push(file);
                    Ok(())
                }
                // 审计报告, 申报稿
                (Some(32), Some(1)) => {
                    file.path.push(DECLARE_SUBFOLDER);
                    file.filename.push_str(date);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.audit_report[0].push(file);
                    Ok(())
                }
                // 审计报告, 上会稿
                (Some(32), Some(2)) => {
                    file.path.push(MEETING_SUBFOLDER);
                    file.filename.push_str(date);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.audit_report[1].push(file);
                    Ok(())
                }
                // 审计报告, 注册稿
                (Some(32), Some(3|4)) => {
                    file.path.push(REGISTER_SUBFOLDER);
                    file.filename.push_str(date);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.audit_report[2].push(file);
                    Ok(())
                }
                // 法律意见书, 申报稿
                (Some(33), Some(1)) => {
                    file.path.push(DECLARE_SUBFOLDER);
                    file.filename.push_str(date);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.legal_opinion[0].push(file);
                    Ok(())
                }
                // 法律意见书, 上会稿
                (Some(33), Some(2)) => {
                    file.path.push(MEETING_SUBFOLDER);
                    file.filename.push_str(date);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.legal_opinion[1].push(file);
                    Ok(())
                }
                // 法律意见书, 注册稿
                (Some(33), Some(3|4)) => {
                    file.path.push(REGISTER_SUBFOLDER);
                    file.filename.push_str(date);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.legal_opinion[2].push(file);
                    Ok(())
                }
                // 其他, 申报稿
                (Some(34), Some(1)) => {
                    file.path.push(DECLARE_SUBFOLDER);
                    file.filename.push_str(date);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.others[0].push(file);
                    Ok(())
                }
                // 其他, 上会稿
                (Some(34), Some(2)) => {
                    file.path.push(MEETING_SUBFOLDER);
                    file.filename.push_str(date);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.others[1].push(file);
                    Ok(())
                }
                // 其他, 注册稿
                (Some(34), Some(3|4)) => {
                    file.path.push(REGISTER_SUBFOLDER);
                    file.filename.push_str(date);
                    file.path.push(&file.filename);
                    file.path.set_extension("pdf");
                    infos.others[2].push(file);
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

impl MeetingAnnounce {
    fn new(resp: String, id: u32) -> Result<MeetingAnnounce, anyhow::Error> {
        let pure_content: Vec<_> = resp.split_inclusive(&['(', ')'][..]).collect();
        #[allow(clippy::useless_format)]
        let mut json_str = format!(r#"{}"#, pure_content[1..].join(""));
        json_str.truncate(json_str.len() - 1);
        let json_body: Value = serde_json::from_str(&json_str)?;
        let mut announce = MeetingAnnounce::default();
        let file_arr = json_body["result"]
            .as_array()
            .context("extract file array failed")?;
        let mut download_base = Url::parse("http://static.sse.com.cn/stock/")?;
        let ret: Result<(), anyhow::Error> = file_arr.iter().try_for_each(|x| {
            let file = UploadFile {
                filename: {
                    let name = x["fileTitle"].as_str().context("get filename failed")?;
                    name.to_owned()
                },
                url: {
                    let download_url = x["filePath"].as_str().context("get file url failed")?;
                    download_base.set_path(&*("stock".to_owned() + download_url));
                    download_base.to_owned()
                    // download_base.join(download_url)?
                },
                path: {
                    let stock_loop = x["stockAudit"].as_array().unwrap();
                    let company_name = {
                        let mut idx: usize = 0;
                        for i in 0..stock_loop.len() {
                            let audit_id = x["stockAudit"][i]["auditId"]
                                .as_str()
                                .unwrap()
                                .parse::<u32>()
                                .unwrap();
                            if audit_id == id {
                                idx = i;
                                break;
                            }
                        }
                        x["stockAudit"][idx]["companyFullName"]
                            .as_str()
                            .context("get company name failed")?
                            .trim()
                    };
                    // x["stockAudit"]
                    // [x["stockAudit"].as_array().unwrap().len() - 1]["companyFullName"]
                    // .as_str()
                    // .context("get company name failed")?;
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

#[derive(Debug, Clone)]
pub struct ReqClient(Client);

impl ReqClient {
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
        ReqClient(client)
    }
}

/// 爬虫入口
#[derive(Debug)]
pub struct SseQuery {
    // reqwest client
    // client: Client,
    // 所有公司信息
    pub companies: Vec<ItemDetail>,
    // 出错的公司名字，需人工处理
    pub failed_logs: Vec<String>,
}

impl SseQuery {
    pub fn new() -> Self {
        Self {
            companies: Vec::new(),
            failed_logs: Vec::new(),
        }
    }

    pub fn add(&mut self, company: std::result::Result<ItemDetail, String>) {
        match company {
            Ok(info) => self.companies.push(info),
            Err(name) => self.failed_logs.push(name),
        }
    }
}

async fn query_company_overview(client: &mut ReqClient, name: &str) -> Result<CompanyInfo, Error> {
    let url = format!("http://query.sse.com.cn/statusAction.do?jsonCallBack=jsonpCallback42305&isPagination=true&sqlId=SH_XM_LB&pageHelp.pageSize=20&offerType=&commitiResult=&registeResult=&province=&csrcCode=&currStatus=&order=&keyword={}&auditApplyDateBegin=&auditApplyDateEnd=&_=1640867539069", name);
    let resp = client.0.get(url).send().await?;

    let body = resp.text().await?;
    Ok(CompanyInfo::try_from(body)?)
}

async fn query_company_disclosure(
    client: &mut ReqClient,
    id: u32,
) -> Result<InfoDisclosure, Error> {
    let url = format!("http://query.sse.com.cn/commonSoaQuery.do?jsonCallBack=jsonpCallback99435173&isPagination=false&sqlId=GP_GPZCZ_SHXXPL&stockAuditNum={}&_=1641094982593", id);
    let resp = client.0.get(url).send().await?;

    let body = resp.text().await?;
    Ok(InfoDisclosure::try_from(body)?)
}

async fn query_company_announce(client: &mut ReqClient, id: u32) -> Result<MeetingAnnounce, Error> {
    let url = format!("http://query.sse.com.cn/commonSoaQuery.do?jsonCallBack=jsonpCallback42495292&isPagination=false&sqlId=GP_GPZCZ_SSWHYGGJG&fileType=1,2,3,4&stockAuditNum={}&_=1641114627446", id);
    let resp = client.0.get(url).send().await?;

    let body = resp.text().await?;
    Ok(MeetingAnnounce::new(body, id)?)
}

pub async fn process_company(
    client: &mut ReqClient,
    name: &str,
) -> std::result::Result<ItemDetail, String> {
    let mut audit_id: u32 = 0;
    let company_info = query_company_overview(client, name).await;
    if company_info.is_ok() {
        audit_id = company_info.as_ref().unwrap().stock_audit_number;
        let disclosure = query_company_disclosure(client, audit_id).await;
        let announce = query_company_announce(client, audit_id).await;
        if disclosure.is_ok() && announce.is_ok() {
            let item = ItemDetail {
                overview: company_info.unwrap(),
                disclosure: disclosure.unwrap(),
                announce: announce.unwrap(),
            };
            // #[cfg(not(test))]
            {
                let ret = download_company_files(client, &item).await;
                match ret {
                    Ok(_) => Ok(item),
                    Err(e) => {
                        let mut err_msg = format!("{}", e);
                        err_msg.push_str(name);
                        Err(err_msg)
                    }
                }
            }
            // #[cfg(test)]
            // Ok(item)
        } else {
            // println!("{:#?}", disclosure);
            // println!("{:#?}", &announce);
            Err(name.to_owned())
        }
    } else {
        println!("{:#?}", company_info);
        Err(name.to_owned())
    }
}

pub async fn download_company_files(
    client: &mut ReqClient,
    company: &ItemDetail,
) -> anyhow::Result<()> {
    let base_folder = &company.overview.stock_audit_name;
    // let client = ReqClient::new();

    // create SUBFOLDERS to save pdf files
    SUBFOLDERS.map(|folder| {
        let sub_folder: PathBuf = ["Download", base_folder, folder]
            .iter()
            .collect::<PathBuf>();
        std::fs::create_dir_all(sub_folder).unwrap_or_else(|why| println!("! {:?}", why.kind()));
    });

    let mut download_tasks = Vec::<(&Url, &PathBuf)>::new();
    company.disclosure.prospectuses.iter().for_each(|x| {
        x.iter().for_each(|y| {
            download_tasks.push((&y.url, &y.path));
        })
    });
    company.disclosure.publish_sponsor.iter().for_each(|x| {
        x.iter().for_each(|y| {
            download_tasks.push((&y.url, &y.path));
        })
    });
    company.disclosure.list_sponsor.iter().for_each(|x| {
        x.iter().for_each(|y| {
            download_tasks.push((&y.url, &y.path));
        })
    });
    company.disclosure.audit_report.iter().for_each(|x| {
        x.iter().for_each(|y| {
            download_tasks.push((&y.url, &y.path));
        })
    });
    company.disclosure.legal_opinion.iter().for_each(|x| {
        x.iter().for_each(|y| {
            download_tasks.push((&y.url, &y.path));
        })
    });
    company.disclosure.others.iter().for_each(|x| {
        x.iter().for_each(|y| {
            download_tasks.push((&y.url, &y.path));
        })
    });
    company.disclosure.query_and_reply.iter().for_each(|x| {
        let y = x.as_ref().unwrap();
        match y {
            QueryReply::Sponsor(z) => download_tasks.push((&z.url, &z.path)),
            QueryReply::Accountant(z) => download_tasks.push((&z.url, &z.path)),
            QueryReply::Lawyer(z) => download_tasks.push((&z.url, &z.path)),
            QueryReply::Other(z) => {
                let sub_folder: PathBuf = ["Download", base_folder, UNCLASSIFIED_SUBFOLDER]
                    .iter()
                    .collect::<PathBuf>();
                std::fs::create_dir_all(sub_folder)
                    .unwrap_or_else(|why| println!("! {:?}", why.kind()));
                download_tasks.push((&z.url, &z.path))
            }
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
    #[cfg(test)]
    println!("{:#?}", download_tasks);
    for (url, path) in download_tasks {
        // println!("{:#?}", url.clone().as_str());
        if !path.exists() {
            let resp = client.0.get(url.clone()).send().await?;
            let content = resp.bytes().await?;
            let mut file = File::create(path).await?;
            file.write_all(&content).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Instant;

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
            r#"jsonpCallback42305({"actionErrors":[],"actionMessages":[],"downloadFileName":null,"execlStream":null,"fieldErrors":{},"fileId":null,"isPagination":"true","jsonCallBack":"jsonpCallback42305","locale":"en_US","mergeSize":null,"mergeType":null,"pageHelp":{"beginPage":1,"cacheSize":1,"data":[{"updateDate":"20211130120921","planIssueCapital":2.7,"suspendStatus":"","wenHao":"","stockAuditName":"广东雅达电子股份有限公司","currStatus":8,"stockAuditNum":"972","registeResult":"","intermediary":[{"auditId":"972","i_intermediaryType":1,"i_intermediaryId":"0_1021","i_person":[{"i_p_personName":"杨雄辉","i_p_jobType":24,"i_p_personId":"732115","i_p_jobTitle":"项目协办人"},{"i_p_personName":"谭星","i_p_jobType":23,"i_p_personId":"732114","i_p_jobTitle":"保荐代表人B"},{"i_p_personName":"文斌","i_p_jobType":22,"i_p_personId":"732113","i_p_jobTitle":"保荐代表人A"},{"i_p_personName":"郜泽民","i_p_jobType":21,"i_p_personId":"732110","i_p_jobTitle":"保荐业务负责�..."#,
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
        // let raw_data = String::from(
        //     r#"jsonpCallback99435173({"actionErrors":[],"actionMessages":[],"fieldErrors":{},"isPagination":"false","jsonCallBack":"jsonpCallback99435173","locale":"en_US","pageHelp":{"beginPage":1,"cacheSize":1,"data":[{"fileUpdateTime":"20220107174000","filePath":"\/information\/c\/202201\/d078b75188094a5d9848073e3099dc97.pdf","publishDate":"2022-01-07","fileLastVersion":1,"stockAuditNum":"1037","auditItemId":"506c1fe4277c4b39aa750b4d3a8fa22b","filename":"d078b75188094a5d9848073e3099dc97.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":638900,"StockType":1,"fileTitle":"8-3 补充法律意见书","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"84021742","OPERATION_SEQ":"c9339ecf660cc1553dcbb668e0f10277"},{"fileUpdateTime":"20220107174000","filePath":"\/information\/c\/202201\/6550a785ae9f4c01bcad74d8ed07e339.pdf","publishDate":"2022-01-07","fileLastVersion":1,"stockAuditNum":"1037","auditItemId":"ab6fe7cdbc194ff985b3f3d7d687f1c5","filename":"6550a785ae9f4c01bcad74d8ed07e339.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":3188045,"StockType":1,"fileTitle":"8-2 会计师关于德邦科技科创板上市首轮问询回复意见","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"84021741","OPERATION_SEQ":"c9339ecf660cc1553dcbb668e0f10277"},{"fileUpdateTime":"20220107174000","filePath":"\/information\/c\/202201\/99858ffdaa4a45c18028374b840b135b.pdf","publishDate":"2022-01-07","fileLastVersion":1,"stockAuditNum":"1037","auditItemId":"3df19f054fe749ecb5ddd1372b26884e","filename":"99858ffdaa4a45c18028374b840b135b.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":3114905,"StockType":1,"fileTitle":"8-1 发行人及保荐机构关于审核问询函的回复(豁免版)","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"84021740","OPERATION_SEQ":"c9339ecf660cc1553dcbb668e0f10277"},{"fileUpdateTime":"20211012170001","filePath":"\/information\/c\/202110\/dbdbfa8875ef40298638549ab1524817.pdf","publishDate":"2021-10-12","fileLastVersion":2,"stockAuditNum":"1037","auditItemId":"c89859bf2b3a11ec9f2608f1ea8a847f","filename":"dbdbfa8875ef40298638549ab1524817.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":11888035,"StockType":1,"fileTitle":"永拓会计师事务所（特殊普通合伙）关于烟台德邦科技股份有限公司首次公开发行股票并在科创板上市的财务报表及审计报告","auditStatus":1,"fileVersion":1,"fileType":32,"MarketType":1,"fileId":"209049","OPERATION_SEQ":"2d2d8d56e0984f7ab38c008edcf491d4"},{"fileUpdateTime":"20211012170001","filePath":"\/information\/c\/202110\/e5b671d8cec14bf3883e8abd45fc3a96.pdf","publishDate":"2021-10-12","fileLastVersion":2,"stockAuditNum":"1037","auditItemId":"c89859bf2b3a11ec9f2608f1ea8a847f","filename":"e5b671d8cec14bf3883e8abd45fc3a96.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":736749,"StockType":1,"fileTitle":"东方证券承销保荐有限公司关于烟台德邦科技股份有限公司首次公开发行股票并在科创板上市的上市保荐书","auditStatus":1,"fileVersion":1,"fileType":37,"MarketType":1,"fileId":"209043","OPERATION_SEQ":"2d2d8d56e0984f7ab38c008edcf491d4"},{"fileUpdateTime":"20211012170001","filePath":"\/information\/c\/202110\/666b8b084d1e4720bc90aa19a40fab5a.pdf","publishDate":"2021-10-12","fileLastVersion":2,"stockAuditNum":"1037","auditItemId":"c89859bf2b3a11ec9f2608f1ea8a847f","filename":"666b8b084d1e4720bc90aa19a40fab5a.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":703086,"StockType":1,"fileTitle":"东方证券承销保荐有限公司关于烟台德邦科技股份有限公司首次公开发行股票并在科创板上市的发行保荐书","auditStatus":1,"fileVersion":1,"fileType":36,"MarketType":1,"fileId":"209041","OPERATION_SEQ":"2d2d8d56e0984f7ab38c008edcf491d4"}],"endDate":null,"endPage":null,"objectResult":null,"pageCount":1,"pageNo":1,"pageSize":6,"searchDate":null,"sort":null,"startDate":null,"total":6},"pageNo":null,"pageSize":null,"queryDate":"","result":[{"fileUpdateTime":"20220107174000","filePath":"\/information\/c\/202201\/d078b75188094a5d9848073e3099dc97.pdf","publishDate":"2022-01-07","fileLastVersion":1,"stockAuditNum":"1037","auditItemId":"506c1fe4277c4b39aa750b4d3a8fa22b","filename":"d078b75188094a5d9848073e3099dc97.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":638900,"StockType":1,"fileTitle":"8-3 补充法律意见书","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"84021742","OPERATION_SEQ":"c9339ecf660cc1553dcbb668e0f10277"},{"fileUpdateTime":"20220107174000","filePath":"\/information\/c\/202201\/6550a785ae9f4c01bcad74d8ed07e339.pdf","publishDate":"2022-01-07","fileLastVersion":1,"stockAuditNum":"1037","auditItemId":"ab6fe7cdbc194ff985b3f3d7d687f1c5","filename":"6550a785ae9f4c01bcad74d8ed07e339.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":3188045,"StockType":1,"fileTitle":"8-2 会计师关于德邦科技科创板上市首轮问询回复意见","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"84021741","OPERATION_SEQ":"c9339ecf660cc1553dcbb668e0f10277"},{"fileUpdateTime":"20220107174000","filePath":"\/information\/c\/202201\/99858ffdaa4a45c18028374b840b135b.pdf","publishDate":"2022-01-07","fileLastVersion":1,"stockAuditNum":"1037","auditItemId":"3df19f054fe749ecb5ddd1372b26884e","filename":"99858ffdaa4a45c18028374b840b135b.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":3114905,"StockType":1,"fileTitle":"8-1 发行人及保荐机构关于审核问询函的回复(豁免版)","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"84021740","OPERATION_SEQ":"c9339ecf660cc1553dcbb668e0f10277"},{"fileUpdateTime":"20211012170001","filePath":"\/information\/c\/202110\/dbdbfa8875ef40298638549ab1524817.pdf","publishDate":"2021-10-12","fileLastVersion":2,"stockAuditNum":"1037","auditItemId":"c89859bf2b3a11ec9f2608f1ea8a847f","filename":"dbdbfa8875ef40298638549ab1524817.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":11888035,"StockType":1,"fileTitle":"永拓会计师事务所（特殊普通合伙）关于烟台德邦科技股份有限公司首次公开发行股票并在科创板上市的财务报表及审计报告","auditStatus":1,"fileVersion":1,"fileType":32,"MarketType":1,"fileId":"209049","OPERATION_SEQ":"2d2d8d56e0984f7ab38c008edcf491d4"},{"fileUpdateTime":"20211012170001","filePath":"\/information\/c\/202110\/e5b671d8cec14bf3883e8abd45fc3a96.pdf","publishDate":"2021-10-12","fileLastVersion":2,"stockAuditNum":"1037","auditItemId":"c89859bf2b3a11ec9f2608f1ea8a847f","filename":"e5b671d8cec14bf3883e8abd45fc3a96.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":736749,"StockType":1,"fileTitle":"东方证券承销保荐有限公司关于烟台德邦科技股份有限公司首次公开发行股票并在科创板上市的上市保荐书","auditStatus":1,"fileVersion":1,"fileType":37,"MarketType":1,"fileId":"209043","OPERATION_SEQ":"2d2d8d56e0984f7ab38c008edcf491d4"},{"fileUpdateTime":"20211012170001","filePath":"\/information\/c\/202110\/666b8b084d1e4720bc90aa19a40fab5a.pdf","publishDate":"2021-10-12","fileLastVersion":2,"stockAuditNum":"1037","auditItemId":"c89859bf2b3a11ec9f2608f1ea8a847f","filename":"666b8b084d1e4720bc90aa19a40fab5a.pdf","companyFullName":"烟台德邦科技股份有限公司","fileSize":703086,"StockType":1,"fileTitle":"东方证券承销保荐有限公司关于烟台德邦科技股份有限公司首次公开发行股票并在科创板上市的发行保荐书","auditStatus":1,"fileVersion":1,"fileType":36,"MarketType":1,"fileId":"209041","OPERATION_SEQ":"2d2d8d56e0984f7ab38c008edcf491d4"}],"securityCode":"","texts":null,"type":"","validateCode":""})"#,
        // );
        let raw_data = String::from(
            r#"jsonpCallback99435173({"actionErrors":[],"actionMessages":[],"fieldErrors":{},"isPagination":"false","jsonCallBack":"jsonpCallback99435173","locale":"en_US","pageHelp":{"beginPage":1,"cacheSize":1,"data":[{"fileUpdateTime":"20210810173001","filePath":"\/information\/c\/202108\/599c224d1d56485ab2594cbb7880d1fb.pdf","publishDate":"2021-07-06","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"893f416ff9bd11eb9f2608f1ea8a847f","filename":"599c224d1d56485ab2594cbb7880d1fb.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":10172385,"StockType":1,"fileTitle":"江苏宏微科技股份有限公司科创板首次公开发行股票招股说明书（注册稿）","auditStatus":5,"fileVersion":4,"fileType":30,"MarketType":1,"fileId":"201168","OPERATION_SEQ":"c27f396abc045c2215d6889f543b109a"},{"fileUpdateTime":"20210810173001","filePath":"\/information\/c\/202108\/cf837f99004d446aa0fb2ad74eda44a1.pdf","publishDate":"2021-07-06","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"893f416ff9bd11eb9f2608f1ea8a847f","filename":"cf837f99004d446aa0fb2ad74eda44a1.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":954978,"StockType":1,"fileTitle":"民生证券股份有限公司关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的发行保荐书","auditStatus":5,"fileVersion":4,"fileType":36,"MarketType":1,"fileId":"201167","OPERATION_SEQ":"c27f396abc045c2215d6889f543b109a"},{"fileUpdateTime":"20210708170000","filePath":"\/information\/c\/202107\/ff98a241cddb451b942291055c3428e7.pdf","publishDate":"2021-07-06","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"a6b4d162c5571ca32a580ab2de8b1034","filename":"ff98a241cddb451b942291055c3428e7.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":383081,"StockType":1,"fileTitle":"关于同意江苏宏微科技股份有限公司首次公开发行股票注册的批复","auditStatus":5,"fileVersion":4,"fileType":35,"MarketType":1,"fileId":"197008a","OPERATION_SEQ":"b845d9aeac87eef43cd894d28daa8c19"},{"fileUpdateTime":"20210706170001","filePath":"\/information\/c\/202107\/72e4fcaa40c441068b5f2a13234d4fa0.pdf","publishDate":"2021-07-06","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"8bc41b27de3811eb9f2608f1ea8a847f","filename":"72e4fcaa40c441068b5f2a13234d4fa0.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":10368026,"StockType":1,"fileTitle":"江苏宏微科技股份有限公司科创板首次公开发行股票招股说明书（注册稿）","auditStatus":4,"fileVersion":3,"fileType":30,"MarketType":1,"fileId":"196724","OPERATION_SEQ":"6ee357caad0c12f7f6d354f223f1052c"},{"fileUpdateTime":"20210706170000","filePath":"\/information\/c\/202107\/788223f9815047c083d36a9e6eb1e31f.pdf","publishDate":"2021-07-06","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"f8dafdd4f0754b72a3bdae8fbfca78b9","filename":"788223f9815047c083d36a9e6eb1e31f.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":490968,"StockType":1,"fileTitle":"8-3 补充法律意见书（五）","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"82213708","OPERATION_SEQ":"b6e5c7a1257596dfe158631a24755b81"},{"fileUpdateTime":"20210706170000","filePath":"\/information\/c\/202107\/9cf6cdf4ebef42a9ad0d8bdc65001bd9.pdf","publishDate":"2021-07-06","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"cf14dff1bf9e43d8832469ed008ef597","filename":"9cf6cdf4ebef42a9ad0d8bdc65001bd9.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":1661881,"StockType":1,"fileTitle":"8-1 发行人及保荐机构关于发行注册环节反馈意见落实函的回复","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"82213707","OPERATION_SEQ":"b6e5c7a1257596dfe158631a24755b81"},{"fileUpdateTime":"20210706170000","filePath":"\/information\/c\/202107\/2ee8e7c97fcf4d8989f742ab2751388e.pdf","publishDate":"2021-07-06","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"7c705ffe18944d379273c162a31a2709","filename":"2ee8e7c97fcf4d8989f742ab2751388e.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":1002971,"StockType":1,"fileTitle":"8-2 会计师关于发行注册环节反馈意见落实函的回复","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"82213706","OPERATION_SEQ":"b6e5c7a1257596dfe158631a24755b81"},{"fileUpdateTime":"20210602193001","filePath":"\/information\/c\/202106\/f2e332223b0048c8a2131eb8350fd42d.pdf","publishDate":"2021-06-02","fileLastVersion":2,"stockAuditNum":"810","auditItemId":"de365d91c39511eb9f2608f1ea8a847f","filename":"f2e332223b0048c8a2131eb8350fd42d.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":6487234,"StockType":1,"fileTitle":"北京市环球律师事务所关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的法律意见书","auditStatus":4,"fileVersion":3,"fileType":33,"MarketType":1,"fileId":"186761","OPERATION_SEQ":"1d877dcb53963b5139caadee89164dd7"},{"fileUpdateTime":"20210602193001","filePath":"\/information\/c\/202106\/247a03c0dd7947a288f1cc7d3d5b3c5c.pdf","publishDate":"2021-06-02","fileLastVersion":2,"stockAuditNum":"810","auditItemId":"de365d91c39511eb9f2608f1ea8a847f","filename":"247a03c0dd7947a288f1cc7d3d5b3c5c.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":9016613,"StockType":1,"fileTitle":"天衡会计师事务所（特殊普通合伙）关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的财务报表及审计报告","auditStatus":4,"fileVersion":3,"fileType":32,"MarketType":1,"fileId":"186760","OPERATION_SEQ":"1d877dcb53963b5139caadee89164dd7"},{"fileUpdateTime":"20210602193001","filePath":"\/information\/c\/202106\/737af05aa1e74ec6b9f11570aef55e4a.pdf","publishDate":"2021-06-02","fileLastVersion":2,"stockAuditNum":"810","auditItemId":"de365d91c39511eb9f2608f1ea8a847f","filename":"737af05aa1e74ec6b9f11570aef55e4a.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":1119016,"StockType":1,"fileTitle":"民生证券股份有限公司关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的上市保荐书","auditStatus":4,"fileVersion":3,"fileType":37,"MarketType":1,"fileId":"186759","OPERATION_SEQ":"1d877dcb53963b5139caadee89164dd7"},{"fileUpdateTime":"20210602193001","filePath":"\/information\/c\/202106\/d436966c9cab4bb4b73761dacc381eed.pdf","publishDate":"2021-06-02","fileLastVersion":2,"stockAuditNum":"810","auditItemId":"de365d91c39511eb9f2608f1ea8a847f","filename":"d436966c9cab4bb4b73761dacc381eed.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":980222,"StockType":1,"fileTitle":"民生证券股份有限公司关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的发行保荐书","auditStatus":4,"fileVersion":3,"fileType":36,"MarketType":1,"fileId":"186758","OPERATION_SEQ":"1d877dcb53963b5139caadee89164dd7"},{"fileUpdateTime":"20210602193001","filePath":"\/information\/c\/202106\/65118a0294ec413cad229b644839c93d.pdf","publishDate":"2021-06-02","fileLastVersion":2,"stockAuditNum":"810","auditItemId":"de365d91c39511eb9f2608f1ea8a847f","filename":"65118a0294ec413cad229b644839c93d.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":10489719,"StockType":1,"fileTitle":"江苏宏微科技股份有限公司科创板首次公开发行股票招股说明书（注册稿）","auditStatus":4,"fileVersion":3,"fileType":30,"MarketType":1,"fileId":"186757","OPERATION_SEQ":"1d877dcb53963b5139caadee89164dd7"},{"fileUpdateTime":"20210525170002","filePath":"\/information\/c\/202105\/c96ebfbe2a7b4aa3b5f4e1ab86e7cd49.pdf","publishDate":"2021-05-25","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"7e30e1b0497c44f4961c4a39681a8b03","filename":"c96ebfbe2a7b4aa3b5f4e1ab86e7cd49.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":726295,"StockType":1,"fileTitle":"8-2 补充法律意见书（四）","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"81756169","OPERATION_SEQ":"5f835f570a4192324e601c45b5fa0f93"},{"fileUpdateTime":"20210525170002","filePath":"\/information\/c\/202105\/15f499bdaeec4f689b60a30180e8aa79.pdf","publishDate":"2021-05-25","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"1aefe06cab3e4816a2724e05e391e057","filename":"15f499bdaeec4f689b60a30180e8aa79.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":1096128,"StockType":1,"fileTitle":"8-1 发行人及保荐机构关于科创板上市委会议意见落实函的回复","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"81756168","OPERATION_SEQ":"5f835f570a4192324e601c45b5fa0f93"},{"fileUpdateTime":"20210511170002","filePath":"\/information\/c\/202105\/cd82936bed7e4f1c830fd68aad4c48ed.pdf","publishDate":"2021-05-11","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"bc9bab4839d54bb69ace30a3ed58ab73","filename":"cd82936bed7e4f1c830fd68aad4c48ed.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":1820723,"StockType":1,"fileTitle":"8-1 发行人及保荐机构关于审核中心意见落实函的回复","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"81566543","OPERATION_SEQ":"599b855f2b2e3bbdfce69e72f02c64f5"},{"fileUpdateTime":"20210511170002","filePath":"\/information\/c\/202105\/f1027d54a4f343588ae935e9adf53607.pdf","publishDate":"2021-05-11","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"d4bd48cabc254343abf0291edb625fb5","filename":"f1027d54a4f343588ae935e9adf53607.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":374621,"StockType":1,"fileTitle":"8-2 会计师关于审核中心意见落实函的回复意见","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"81566542","OPERATION_SEQ":"599b855f2b2e3bbdfce69e72f02c64f5"},{"fileUpdateTime":"20210511170002","filePath":"\/information\/c\/202105\/bb2ccbdaf64242fba3c63472874a90a3.pdf","publishDate":"2021-05-11","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"45661c91b23711eb9f2608f1ea8a847f","filename":"bb2ccbdaf64242fba3c63472874a90a3.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":1045163,"StockType":1,"fileTitle":"民生证券股份有限公司关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的发行保荐书","auditStatus":2,"fileVersion":2,"fileType":36,"MarketType":1,"fileId":"183301","OPERATION_SEQ":"599b855f2b2e3bbdfce69e72f02c64f5"},{"fileUpdateTime":"20210511170002","filePath":"\/information\/c\/202105\/15b94a71682d465b837f05dcc35c1610.pdf","publishDate":"2021-05-11","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"45661c91b23711eb9f2608f1ea8a847f","filename":"15b94a71682d465b837f05dcc35c1610.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":1187910,"StockType":1,"fileTitle":"民生证券股份有限公司关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的上市保荐书","auditStatus":2,"fileVersion":2,"fileType":37,"MarketType":1,"fileId":"183300","OPERATION_SEQ":"599b855f2b2e3bbdfce69e72f02c64f5"},{"fileUpdateTime":"20210511170002","filePath":"\/information\/c\/202105\/3d514061b4be4cae9f087e803e85e98c.pdf","publishDate":"2021-05-11","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"45661c91b23711eb9f2608f1ea8a847f","filename":"3d514061b4be4cae9f087e803e85e98c.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":9067755,"StockType":1,"fileTitle":"天衡会计师事务所（特殊普通合伙）关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的财务报表及审计报告","auditStatus":2,"fileVersion":2,"fileType":32,"MarketType":1,"fileId":"183297","OPERATION_SEQ":"599b855f2b2e3bbdfce69e72f02c64f5"},{"fileUpdateTime":"20210511170002","filePath":"\/information\/c\/202105\/59bea2dc0ceb4e7c8dc0e9c5764e7143.pdf","publishDate":"2021-05-11","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"45661c91b23711eb9f2608f1ea8a847f","filename":"59bea2dc0ceb4e7c8dc0e9c5764e7143.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":6469060,"StockType":1,"fileTitle":"北京市环球律师事务所关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的法律意见书","auditStatus":2,"fileVersion":2,"fileType":33,"MarketType":1,"fileId":"183296","OPERATION_SEQ":"599b855f2b2e3bbdfce69e72f02c64f5"},{"fileUpdateTime":"20210511170002","filePath":"\/information\/c\/202105\/22013426b5d54b52a9261b962b7e5b93.pdf","publishDate":"2021-05-11","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"45661c91b23711eb9f2608f1ea8a847f","filename":"22013426b5d54b52a9261b962b7e5b93.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":10516113,"StockType":1,"fileTitle":"江苏宏微科技股份有限公司科创板首次公开发行股票招股说明书（上会稿）","auditStatus":2,"fileVersion":2,"fileType":30,"MarketType":1,"fileId":"183295","OPERATION_SEQ":"599b855f2b2e3bbdfce69e72f02c64f5"},{"fileUpdateTime":"20210430170000","filePath":"\/information\/c\/202104\/dea5d43fade44cc2943be9cdbbdc607e.pdf","publishDate":"2021-04-30","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"7040c835260c4b18b271a5d1dae69e08","filename":"dea5d43fade44cc2943be9cdbbdc607e.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":5064951,"StockType":1,"fileTitle":"8-1 发行人及保荐机构关于第二轮审核问询函的回复","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"81422143","OPERATION_SEQ":"2607738008ef082fb48cf33d489c254e"},{"fileUpdateTime":"20210430170000","filePath":"\/information\/c\/202104\/2ac632ac4ed94e018cc55c8bad02664f.pdf","publishDate":"2021-04-30","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"3ee737f8a72c450ab96c80e927e31008","filename":"2ac632ac4ed94e018cc55c8bad02664f.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":2555406,"StockType":1,"fileTitle":"8-2 申报会计师关于第二轮审核问询函的回复","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"81422142","OPERATION_SEQ":"2607738008ef082fb48cf33d489c254e"},{"fileUpdateTime":"20210430170000","filePath":"\/information\/c\/202104\/db38364be9b5450fbdd101d664b502dc.pdf","publishDate":"2021-04-30","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"f061b17593eb4fdfbd77e0c8dfd99972","filename":"db38364be9b5450fbdd101d664b502dc.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":1841288,"StockType":1,"fileTitle":"8-3 补充法律意见书（二）","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"81422141","OPERATION_SEQ":"2607738008ef082fb48cf33d489c254e"},{"fileUpdateTime":"20210322170002","filePath":"\/information\/c\/202103\/5dbf352865d9464389e829ff4af2472a.pdf","publishDate":"2021-03-22","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"aa3ee305e01c4b1fa40e11ac14a1a59a","filename":"5dbf352865d9464389e829ff4af2472a.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":5693871,"StockType":1,"fileTitle":"8-1 关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市申请文件的审核问询函的回复","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"80910221","OPERATION_SEQ":"e499a9282b779b0bf08ad04e2a70a683"},{"fileUpdateTime":"20210322170002","filePath":"\/information\/c\/202103\/62808134a1644a96a8b699f96bf78625.pdf","publishDate":"2021-03-22","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"9a532b5c385c4439bdb29362935d794c","filename":"62808134a1644a96a8b699f96bf78625.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":5134566,"StockType":1,"fileTitle":"8-2 申报会计师关于审核问询函的回复","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"80910220","OPERATION_SEQ":"e499a9282b779b0bf08ad04e2a70a683"},{"fileUpdateTime":"20210322170002","filePath":"\/information\/c\/202103\/42e557e5e8e14714b287eb038623519f.pdf","publishDate":"2021-03-22","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"fc9d7774c9c44cb793683f69a023ed51","filename":"42e557e5e8e14714b287eb038623519f.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":2265743,"StockType":1,"fileTitle":"8-3 补充法律意见书（一）","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"80910219","OPERATION_SEQ":"e499a9282b779b0bf08ad04e2a70a683"},{"fileUpdateTime":"20201222170002","filePath":"\/information\/c\/202012\/1b9b9bff9cff46969bda6645a66c7400.pdf","publishDate":"2020-12-22","fileLastVersion":2,"stockAuditNum":"810","auditItemId":"13e4eadf443411ebb0c708f1ea8a847f","filename":"1b9b9bff9cff46969bda6645a66c7400.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":988253,"StockType":1,"fileTitle":"北京市环球律师事务所关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的法律意见书","auditStatus":1,"fileVersion":1,"fileType":33,"MarketType":1,"fileId":"161406","OPERATION_SEQ":"e86334eb393c8e30c7899e78da9f9769"},{"fileUpdateTime":"20201222170002","filePath":"\/information\/c\/202012\/4066ca100a0a4d169fb37549704a7ff6.pdf","publishDate":"2020-12-22","fileLastVersion":2,"stockAuditNum":"810","auditItemId":"13e4eadf443411ebb0c708f1ea8a847f","filename":"4066ca100a0a4d169fb37549704a7ff6.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":8338127,"StockType":1,"fileTitle":"天衡会计师事务所（特殊普通合伙）关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的财务报表及审计报告","auditStatus":1,"fileVersion":1,"fileType":32,"MarketType":1,"fileId":"161402","OPERATION_SEQ":"e86334eb393c8e30c7899e78da9f9769"},{"fileUpdateTime":"20201222170002","filePath":"\/information\/c\/202012\/94633c4e1a35498fa1910665b2c57af4.pdf","publishDate":"2020-12-22","fileLastVersion":2,"stockAuditNum":"810","auditItemId":"13e4eadf443411ebb0c708f1ea8a847f","filename":"94633c4e1a35498fa1910665b2c57af4.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":656431,"StockType":1,"fileTitle":"民生证券股份有限公司关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的上市保荐书","auditStatus":1,"fileVersion":1,"fileType":37,"MarketType":1,"fileId":"161396","OPERATION_SEQ":"e86334eb393c8e30c7899e78da9f9769"},{"fileUpdateTime":"20201222170002","filePath":"\/information\/c\/202012\/e9f04755ec57457b9bb25d449c400101.pdf","publishDate":"2020-12-22","fileLastVersion":2,"stockAuditNum":"810","auditItemId":"13e4eadf443411ebb0c708f1ea8a847f","filename":"e9f04755ec57457b9bb25d449c400101.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":673602,"StockType":1,"fileTitle":"民生证券股份有限公司关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的发行保荐书","auditStatus":1,"fileVersion":1,"fileType":36,"MarketType":1,"fileId":"161394","OPERATION_SEQ":"e86334eb393c8e30c7899e78da9f9769"},{"fileUpdateTime":"20201222170002","filePath":"\/information\/c\/202012\/529b5b19d2dd4fa9bd3a26dd5bb710f7.pdf","publishDate":"2020-12-22","fileLastVersion":2,"stockAuditNum":"810","auditItemId":"13e4eadf443411ebb0c708f1ea8a847f","filename":"529b5b19d2dd4fa9bd3a26dd5bb710f7.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":5038891,"StockType":1,"fileTitle":"江苏宏微科技股份有限公司科创板首次公开发行股票招股说明书（申报稿）","auditStatus":1,"fileVersion":1,"fileType":30,"MarketType":1,"fileId":"161387","OPERATION_SEQ":"e86334eb393c8e30c7899e78da9f9769"}],"endDate":null,"endPage":null,"objectResult":null,"pageCount":1,"pageNo":1,"pageSize":32,"searchDate":null,"sort":null,"startDate":null,"total":32},"pageNo":null,"pageSize":null,"queryDate":"","result":[{"fileUpdateTime":"20210810173001","filePath":"\/information\/c\/202108\/599c224d1d56485ab2594cbb7880d1fb.pdf","publishDate":"2021-07-06","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"893f416ff9bd11eb9f2608f1ea8a847f","filename":"599c224d1d56485ab2594cbb7880d1fb.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":10172385,"StockType":1,"fileTitle":"江苏宏微科技股份有限公司科创板首次公开发行股票招股说明书（注册稿）","auditStatus":5,"fileVersion":4,"fileType":30,"MarketType":1,"fileId":"201168","OPERATION_SEQ":"c27f396abc045c2215d6889f543b109a"},{"fileUpdateTime":"20210810173001","filePath":"\/information\/c\/202108\/cf837f99004d446aa0fb2ad74eda44a1.pdf","publishDate":"2021-07-06","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"893f416ff9bd11eb9f2608f1ea8a847f","filename":"cf837f99004d446aa0fb2ad74eda44a1.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":954978,"StockType":1,"fileTitle":"民生证券股份有限公司关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的发行保荐书","auditStatus":5,"fileVersion":4,"fileType":36,"MarketType":1,"fileId":"201167","OPERATION_SEQ":"c27f396abc045c2215d6889f543b109a"},{"fileUpdateTime":"20210708170000","filePath":"\/information\/c\/202107\/ff98a241cddb451b942291055c3428e7.pdf","publishDate":"2021-07-06","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"a6b4d162c5571ca32a580ab2de8b1034","filename":"ff98a241cddb451b942291055c3428e7.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":383081,"StockType":1,"fileTitle":"关于同意江苏宏微科技股份有限公司首次公开发行股票注册的批复","auditStatus":5,"fileVersion":4,"fileType":35,"MarketType":1,"fileId":"197008a","OPERATION_SEQ":"b845d9aeac87eef43cd894d28daa8c19"},{"fileUpdateTime":"20210706170001","filePath":"\/information\/c\/202107\/72e4fcaa40c441068b5f2a13234d4fa0.pdf","publishDate":"2021-07-06","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"8bc41b27de3811eb9f2608f1ea8a847f","filename":"72e4fcaa40c441068b5f2a13234d4fa0.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":10368026,"StockType":1,"fileTitle":"江苏宏微科技股份有限公司科创板首次公开发行股票招股说明书（注册稿）","auditStatus":4,"fileVersion":3,"fileType":30,"MarketType":1,"fileId":"196724","OPERATION_SEQ":"6ee357caad0c12f7f6d354f223f1052c"},{"fileUpdateTime":"20210706170000","filePath":"\/information\/c\/202107\/788223f9815047c083d36a9e6eb1e31f.pdf","publishDate":"2021-07-06","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"f8dafdd4f0754b72a3bdae8fbfca78b9","filename":"788223f9815047c083d36a9e6eb1e31f.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":490968,"StockType":1,"fileTitle":"8-3 补充法律意见书（五）","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"82213708","OPERATION_SEQ":"b6e5c7a1257596dfe158631a24755b81"},{"fileUpdateTime":"20210706170000","filePath":"\/information\/c\/202107\/9cf6cdf4ebef42a9ad0d8bdc65001bd9.pdf","publishDate":"2021-07-06","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"cf14dff1bf9e43d8832469ed008ef597","filename":"9cf6cdf4ebef42a9ad0d8bdc65001bd9.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":1661881,"StockType":1,"fileTitle":"8-1 发行人及保荐机构关于发行注册环节反馈意见落实函的回复","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"82213707","OPERATION_SEQ":"b6e5c7a1257596dfe158631a24755b81"},{"fileUpdateTime":"20210706170000","filePath":"\/information\/c\/202107\/2ee8e7c97fcf4d8989f742ab2751388e.pdf","publishDate":"2021-07-06","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"7c705ffe18944d379273c162a31a2709","filename":"2ee8e7c97fcf4d8989f742ab2751388e.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":1002971,"StockType":1,"fileTitle":"8-2 会计师关于发行注册环节反馈意见落实函的回复","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"82213706","OPERATION_SEQ":"b6e5c7a1257596dfe158631a24755b81"},{"fileUpdateTime":"20210602193001","filePath":"\/information\/c\/202106\/f2e332223b0048c8a2131eb8350fd42d.pdf","publishDate":"2021-06-02","fileLastVersion":2,"stockAuditNum":"810","auditItemId":"de365d91c39511eb9f2608f1ea8a847f","filename":"f2e332223b0048c8a2131eb8350fd42d.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":6487234,"StockType":1,"fileTitle":"北京市环球律师事务所关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的法律意见书","auditStatus":4,"fileVersion":3,"fileType":33,"MarketType":1,"fileId":"186761","OPERATION_SEQ":"1d877dcb53963b5139caadee89164dd7"},{"fileUpdateTime":"20210602193001","filePath":"\/information\/c\/202106\/247a03c0dd7947a288f1cc7d3d5b3c5c.pdf","publishDate":"2021-06-02","fileLastVersion":2,"stockAuditNum":"810","auditItemId":"de365d91c39511eb9f2608f1ea8a847f","filename":"247a03c0dd7947a288f1cc7d3d5b3c5c.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":9016613,"StockType":1,"fileTitle":"天衡会计师事务所（特殊普通合伙）关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的财务报表及审计报告","auditStatus":4,"fileVersion":3,"fileType":32,"MarketType":1,"fileId":"186760","OPERATION_SEQ":"1d877dcb53963b5139caadee89164dd7"},{"fileUpdateTime":"20210602193001","filePath":"\/information\/c\/202106\/737af05aa1e74ec6b9f11570aef55e4a.pdf","publishDate":"2021-06-02","fileLastVersion":2,"stockAuditNum":"810","auditItemId":"de365d91c39511eb9f2608f1ea8a847f","filename":"737af05aa1e74ec6b9f11570aef55e4a.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":1119016,"StockType":1,"fileTitle":"民生证券股份有限公司关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的上市保荐书","auditStatus":4,"fileVersion":3,"fileType":37,"MarketType":1,"fileId":"186759","OPERATION_SEQ":"1d877dcb53963b5139caadee89164dd7"},{"fileUpdateTime":"20210602193001","filePath":"\/information\/c\/202106\/d436966c9cab4bb4b73761dacc381eed.pdf","publishDate":"2021-06-02","fileLastVersion":2,"stockAuditNum":"810","auditItemId":"de365d91c39511eb9f2608f1ea8a847f","filename":"d436966c9cab4bb4b73761dacc381eed.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":980222,"StockType":1,"fileTitle":"民生证券股份有限公司关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的发行保荐书","auditStatus":4,"fileVersion":3,"fileType":36,"MarketType":1,"fileId":"186758","OPERATION_SEQ":"1d877dcb53963b5139caadee89164dd7"},{"fileUpdateTime":"20210602193001","filePath":"\/information\/c\/202106\/65118a0294ec413cad229b644839c93d.pdf","publishDate":"2021-06-02","fileLastVersion":2,"stockAuditNum":"810","auditItemId":"de365d91c39511eb9f2608f1ea8a847f","filename":"65118a0294ec413cad229b644839c93d.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":10489719,"StockType":1,"fileTitle":"江苏宏微科技股份有限公司科创板首次公开发行股票招股说明书（注册稿）","auditStatus":4,"fileVersion":3,"fileType":30,"MarketType":1,"fileId":"186757","OPERATION_SEQ":"1d877dcb53963b5139caadee89164dd7"},{"fileUpdateTime":"20210525170002","filePath":"\/information\/c\/202105\/c96ebfbe2a7b4aa3b5f4e1ab86e7cd49.pdf","publishDate":"2021-05-25","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"7e30e1b0497c44f4961c4a39681a8b03","filename":"c96ebfbe2a7b4aa3b5f4e1ab86e7cd49.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":726295,"StockType":1,"fileTitle":"8-2 补充法律意见书（四）","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"81756169","OPERATION_SEQ":"5f835f570a4192324e601c45b5fa0f93"},{"fileUpdateTime":"20210525170002","filePath":"\/information\/c\/202105\/15f499bdaeec4f689b60a30180e8aa79.pdf","publishDate":"2021-05-25","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"1aefe06cab3e4816a2724e05e391e057","filename":"15f499bdaeec4f689b60a30180e8aa79.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":1096128,"StockType":1,"fileTitle":"8-1 发行人及保荐机构关于科创板上市委会议意见落实函的回复","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"81756168","OPERATION_SEQ":"5f835f570a4192324e601c45b5fa0f93"},{"fileUpdateTime":"20210511170002","filePath":"\/information\/c\/202105\/cd82936bed7e4f1c830fd68aad4c48ed.pdf","publishDate":"2021-05-11","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"bc9bab4839d54bb69ace30a3ed58ab73","filename":"cd82936bed7e4f1c830fd68aad4c48ed.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":1820723,"StockType":1,"fileTitle":"8-1 发行人及保荐机构关于审核中心意见落实函的回复","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"81566543","OPERATION_SEQ":"599b855f2b2e3bbdfce69e72f02c64f5"},{"fileUpdateTime":"20210511170002","filePath":"\/information\/c\/202105\/f1027d54a4f343588ae935e9adf53607.pdf","publishDate":"2021-05-11","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"d4bd48cabc254343abf0291edb625fb5","filename":"f1027d54a4f343588ae935e9adf53607.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":374621,"StockType":1,"fileTitle":"8-2 会计师关于审核中心意见落实函的回复意见","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"81566542","OPERATION_SEQ":"599b855f2b2e3bbdfce69e72f02c64f5"},{"fileUpdateTime":"20210511170002","filePath":"\/information\/c\/202105\/bb2ccbdaf64242fba3c63472874a90a3.pdf","publishDate":"2021-05-11","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"45661c91b23711eb9f2608f1ea8a847f","filename":"bb2ccbdaf64242fba3c63472874a90a3.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":1045163,"StockType":1,"fileTitle":"民生证券股份有限公司关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的发行保荐书","auditStatus":2,"fileVersion":2,"fileType":36,"MarketType":1,"fileId":"183301","OPERATION_SEQ":"599b855f2b2e3bbdfce69e72f02c64f5"},{"fileUpdateTime":"20210511170002","filePath":"\/information\/c\/202105\/15b94a71682d465b837f05dcc35c1610.pdf","publishDate":"2021-05-11","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"45661c91b23711eb9f2608f1ea8a847f","filename":"15b94a71682d465b837f05dcc35c1610.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":1187910,"StockType":1,"fileTitle":"民生证券股份有限公司关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的上市保荐书","auditStatus":2,"fileVersion":2,"fileType":37,"MarketType":1,"fileId":"183300","OPERATION_SEQ":"599b855f2b2e3bbdfce69e72f02c64f5"},{"fileUpdateTime":"20210511170002","filePath":"\/information\/c\/202105\/3d514061b4be4cae9f087e803e85e98c.pdf","publishDate":"2021-05-11","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"45661c91b23711eb9f2608f1ea8a847f","filename":"3d514061b4be4cae9f087e803e85e98c.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":9067755,"StockType":1,"fileTitle":"天衡会计师事务所（特殊普通合伙）关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的财务报表及审计报告","auditStatus":2,"fileVersion":2,"fileType":32,"MarketType":1,"fileId":"183297","OPERATION_SEQ":"599b855f2b2e3bbdfce69e72f02c64f5"},{"fileUpdateTime":"20210511170002","filePath":"\/information\/c\/202105\/59bea2dc0ceb4e7c8dc0e9c5764e7143.pdf","publishDate":"2021-05-11","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"45661c91b23711eb9f2608f1ea8a847f","filename":"59bea2dc0ceb4e7c8dc0e9c5764e7143.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":6469060,"StockType":1,"fileTitle":"北京市环球律师事务所关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的法律意见书","auditStatus":2,"fileVersion":2,"fileType":33,"MarketType":1,"fileId":"183296","OPERATION_SEQ":"599b855f2b2e3bbdfce69e72f02c64f5"},{"fileUpdateTime":"20210511170002","filePath":"\/information\/c\/202105\/22013426b5d54b52a9261b962b7e5b93.pdf","publishDate":"2021-05-11","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"45661c91b23711eb9f2608f1ea8a847f","filename":"22013426b5d54b52a9261b962b7e5b93.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":10516113,"StockType":1,"fileTitle":"江苏宏微科技股份有限公司科创板首次公开发行股票招股说明书（上会稿）","auditStatus":2,"fileVersion":2,"fileType":30,"MarketType":1,"fileId":"183295","OPERATION_SEQ":"599b855f2b2e3bbdfce69e72f02c64f5"},{"fileUpdateTime":"20210430170000","filePath":"\/information\/c\/202104\/dea5d43fade44cc2943be9cdbbdc607e.pdf","publishDate":"2021-04-30","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"7040c835260c4b18b271a5d1dae69e08","filename":"dea5d43fade44cc2943be9cdbbdc607e.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":5064951,"StockType":1,"fileTitle":"8-1 发行人及保荐机构关于第二轮审核问询函的回复","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"81422143","OPERATION_SEQ":"2607738008ef082fb48cf33d489c254e"},{"fileUpdateTime":"20210430170000","filePath":"\/information\/c\/202104\/2ac632ac4ed94e018cc55c8bad02664f.pdf","publishDate":"2021-04-30","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"3ee737f8a72c450ab96c80e927e31008","filename":"2ac632ac4ed94e018cc55c8bad02664f.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":2555406,"StockType":1,"fileTitle":"8-2 申报会计师关于第二轮审核问询函的回复","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"81422142","OPERATION_SEQ":"2607738008ef082fb48cf33d489c254e"},{"fileUpdateTime":"20210430170000","filePath":"\/information\/c\/202104\/db38364be9b5450fbdd101d664b502dc.pdf","publishDate":"2021-04-30","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"f061b17593eb4fdfbd77e0c8dfd99972","filename":"db38364be9b5450fbdd101d664b502dc.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":1841288,"StockType":1,"fileTitle":"8-3 补充法律意见书（二）","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"81422141","OPERATION_SEQ":"2607738008ef082fb48cf33d489c254e"},{"fileUpdateTime":"20210322170002","filePath":"\/information\/c\/202103\/5dbf352865d9464389e829ff4af2472a.pdf","publishDate":"2021-03-22","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"aa3ee305e01c4b1fa40e11ac14a1a59a","filename":"5dbf352865d9464389e829ff4af2472a.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":5693871,"StockType":1,"fileTitle":"8-1 关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市申请文件的审核问询函的回复","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"80910221","OPERATION_SEQ":"e499a9282b779b0bf08ad04e2a70a683"},{"fileUpdateTime":"20210322170002","filePath":"\/information\/c\/202103\/62808134a1644a96a8b699f96bf78625.pdf","publishDate":"2021-03-22","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"9a532b5c385c4439bdb29362935d794c","filename":"62808134a1644a96a8b699f96bf78625.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":5134566,"StockType":1,"fileTitle":"8-2 申报会计师关于审核问询函的回复","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"80910220","OPERATION_SEQ":"e499a9282b779b0bf08ad04e2a70a683"},{"fileUpdateTime":"20210322170002","filePath":"\/information\/c\/202103\/42e557e5e8e14714b287eb038623519f.pdf","publishDate":"2021-03-22","fileLastVersion":1,"stockAuditNum":"810","auditItemId":"fc9d7774c9c44cb793683f69a023ed51","filename":"42e557e5e8e14714b287eb038623519f.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":2265743,"StockType":1,"fileTitle":"8-3 补充法律意见书（一）","auditStatus":1,"fileVersion":1,"fileType":6,"MarketType":1,"fileId":"80910219","OPERATION_SEQ":"e499a9282b779b0bf08ad04e2a70a683"},{"fileUpdateTime":"20201222170002","filePath":"\/information\/c\/202012\/1b9b9bff9cff46969bda6645a66c7400.pdf","publishDate":"2020-12-22","fileLastVersion":2,"stockAuditNum":"810","auditItemId":"13e4eadf443411ebb0c708f1ea8a847f","filename":"1b9b9bff9cff46969bda6645a66c7400.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":988253,"StockType":1,"fileTitle":"北京市环球律师事务所关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的法律意见书","auditStatus":1,"fileVersion":1,"fileType":33,"MarketType":1,"fileId":"161406","OPERATION_SEQ":"e86334eb393c8e30c7899e78da9f9769"},{"fileUpdateTime":"20201222170002","filePath":"\/information\/c\/202012\/4066ca100a0a4d169fb37549704a7ff6.pdf","publishDate":"2020-12-22","fileLastVersion":2,"stockAuditNum":"810","auditItemId":"13e4eadf443411ebb0c708f1ea8a847f","filename":"4066ca100a0a4d169fb37549704a7ff6.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":8338127,"StockType":1,"fileTitle":"天衡会计师事务所（特殊普通合伙）关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的财务报表及审计报告","auditStatus":1,"fileVersion":1,"fileType":32,"MarketType":1,"fileId":"161402","OPERATION_SEQ":"e86334eb393c8e30c7899e78da9f9769"},{"fileUpdateTime":"20201222170002","filePath":"\/information\/c\/202012\/94633c4e1a35498fa1910665b2c57af4.pdf","publishDate":"2020-12-22","fileLastVersion":2,"stockAuditNum":"810","auditItemId":"13e4eadf443411ebb0c708f1ea8a847f","filename":"94633c4e1a35498fa1910665b2c57af4.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":656431,"StockType":1,"fileTitle":"民生证券股份有限公司关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的上市保荐书","auditStatus":1,"fileVersion":1,"fileType":37,"MarketType":1,"fileId":"161396","OPERATION_SEQ":"e86334eb393c8e30c7899e78da9f9769"},{"fileUpdateTime":"20201222170002","filePath":"\/information\/c\/202012\/e9f04755ec57457b9bb25d449c400101.pdf","publishDate":"2020-12-22","fileLastVersion":2,"stockAuditNum":"810","auditItemId":"13e4eadf443411ebb0c708f1ea8a847f","filename":"e9f04755ec57457b9bb25d449c400101.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":673602,"StockType":1,"fileTitle":"民生证券股份有限公司关于江苏宏微科技股份有限公司首次公开发行股票并在科创板上市的发行保荐书","auditStatus":1,"fileVersion":1,"fileType":36,"MarketType":1,"fileId":"161394","OPERATION_SEQ":"e86334eb393c8e30c7899e78da9f9769"},{"fileUpdateTime":"20201222170002","filePath":"\/information\/c\/202012\/529b5b19d2dd4fa9bd3a26dd5bb710f7.pdf","publishDate":"2020-12-22","fileLastVersion":2,"stockAuditNum":"810","auditItemId":"13e4eadf443411ebb0c708f1ea8a847f","filename":"529b5b19d2dd4fa9bd3a26dd5bb710f7.pdf","companyFullName":"江苏宏微科技股份有限公司","fileSize":5038891,"StockType":1,"fileTitle":"江苏宏微科技股份有限公司科创板首次公开发行股票招股说明书（申报稿）","auditStatus":1,"fileVersion":1,"fileType":30,"MarketType":1,"fileId":"161387","OPERATION_SEQ":"e86334eb393c8e30c7899e78da9f9769"}],"securityCode":"","texts":null,"type":"","validateCode":""})"#,
        );
        let disclosure = InfoDisclosure::try_from(raw_data);
        println!("{:#?}", disclosure);
    }

    #[tokio::test]
    async fn test_query_company_info() {
        let mut client = ReqClient::new();
        let company = query_company_overview(&mut client, "大汉软件股份有限公司")
            .await
            .unwrap();
        println!("{:#?}", company)
    }

    #[tokio::test]
    async fn test_query_company_disclosure() {
        let mut client = ReqClient::new();
        let info = query_company_disclosure(&mut client, 759).await.unwrap();
        println!("{:#?}", info)
    }

    #[tokio::test]
    async fn test_query_company_announce() {
        let mut client = ReqClient::new();
        let announce = query_company_announce(&mut client, 759).await.unwrap();
        println!("{:#?}", announce)
    }

    #[tokio::test]
    async fn test_process_more_companies() {
        // let mut sse = Arc::new(Mutex::new(SseCrawler::new()));
        let now = Instant::now();
        let mut client = ReqClient::new();
        let mut sse = SseQuery::new();
        let companies = [
            "上海赛伦生物技术股份有限公司",
            "大汉软件股份有限公司",
            "浙江海正生物材料股份有限公司",
            "江苏集萃药康生物科技股份有限公司",
        ];
        for i in 0..companies.len() {
            let info = process_company(&mut client, companies[i]).await;
            download_company_files(&mut client, &info.as_ref().unwrap())
                .await
                .unwrap();
            sse.add(info);
        }
        println!("{:#?}", sse);
        println!("总耗时：{} ms", now.elapsed().as_millis());
    }

    #[tokio::test]
    async fn test_process_more_companies_true_async() {
        let now = Instant::now();
        let mut sse = Arc::new(Mutex::new(SseQuery::new()));
        let companies = [
            "上海赛伦生物技术股份有限公司",
            "大汉软件股份有限公司",
            "浙江海正生物材料股份有限公司",
            "江苏集萃药康生物科技股份有限公司",
        ];
        let mut handles = Vec::with_capacity(companies.len());
        for i in 0..companies.len() {
            let sse_copy = sse.clone();
            handles.push(tokio::spawn(async move {
                let mut client = ReqClient::new();
                let ret = process_company(&mut client, companies[i]).await;
                download_company_files(&mut client, &ret.as_ref().unwrap()).await;
                let mut copy = sse_copy.lock().await;
                copy.add(ret);
            }));
        }
        for handle in handles {
            handle.await;
        }

        // sleep(Duration::from_secs(20)).await;
        println!("{:#?}", sse);
        println!("总耗时：{} ms", now.elapsed().as_millis());
    }

    #[tokio::test]
    async fn test_create_subfolder() {
        let mut sse = SseQuery::new();
        let mut client = ReqClient::new();
        let item = process_company(&mut client, "大汉软件股份有限公司").await;
        sse.add(item);
        download_company_files(&mut client, &sse.companies[0]).await;
        // println!("{:#?}", sse);
    }

    #[tokio::test]
    async fn test_control_concurrency_num() {
        let now = Instant::now();
        let mut sse = Arc::new(Mutex::new(SseQuery::new()));
        let companies = [
            "上海赛伦生物技术股份有限公司",
            "大汉软件股份有限公司",
            "浙江海正生物材料股份有限公司",
            "江苏集萃药康生物科技股份有限公司",
        ];
        let idx: Vec<usize> = (0..companies.len()).collect();
        for chunk in idx.chunks(2) {
            let mut handles = Vec::with_capacity(2);
            for &elem in chunk.iter() {
                let sse_copy = sse.clone();
                handles.push(tokio::spawn(async move {
                    let mut client = ReqClient::new();
                    let ret = process_company(&mut client, companies[elem]).await;
                    download_company_files(&mut client, &ret.as_ref().unwrap()).await;
                    let mut copy = sse_copy.lock().await;
                    copy.add(ret);
                }));
            }
            for handle in handles {
                handle.await;
            }
            println!("耗时：{} ms", now.elapsed().as_millis());
        }

        // sleep(Duration::from_secs(20)).await;
        // println!("{:#?}", sse);
        println!("总耗时：{} ms", now.elapsed().as_millis());
    }

    #[tokio::test]
    async fn test_process_company() {
        let mut client = ReqClient::new();
        // let sse = process_company(&mut client, "亚信安全科技股份有限公司").await;
        let sse = process_company(&mut client, "江苏宏微科技股份有限公司").await;
        println!("{:#?}", sse);
    }
}
